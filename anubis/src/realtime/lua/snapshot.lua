-- How some channels' streams open, decided and read at one instant.
--
-- The counterpart of `anubis::realtime::replay::MemoryLog::snapshot`, applying
-- the resume rules of `MemoryLog::opening`, kept in step with it;
-- `docs/realtime.md` states them. One script decides each channel's opening
-- from the head, the watermark, and the channel's floor, then reads only the
-- entries that opening sends, so nothing published while it runs can land
-- between two of them, and a cursor that cannot be vouched for costs nothing
-- to read. Sent after log.lua.
--
-- KEYS: 1 the log hash, 2 the recency sorted set, 3 the floors hash, 4 the
--       sizes hash, 5 the list of retired epochs.
-- ARGV: 1 a candidate epoch, used only when the log starts a new one, 2 the
--       prefix every key shares, then five per channel: its name; where it
--       begins, "fresh", "unknown", or "after" a cursor; the cursor's epoch
--       and sequence, which only "after" reads; and "1" for a backfill
--       stream, "0" otherwise.
--
-- Returns { { epoch, head }, { { reset, { entry, ... } }, ... } }, one opening
-- per channel in the order they were named, where reset is 1 when the channel
-- opens with a reset frame.

local log, recent, floors, sizes, retired = KEYS[1], KEYS[2], KEYS[3], KEYS[4], KEYS[5]
local prefix = ARGV[2]

-- The entries of one buffer after `since`, oldest first.
--
-- Entries are in sequence order, so the ones after `since` are a run at the
-- tail. The tail is read in pages that double, newest first, and the walk
-- stops at the first entry already heard, so a resume near the head reads a
-- page rather than the whole buffer.
local function entries_after(key, since)
  local length = redis.call('LLEN', key)
  local newest_first = {}
  local stop, page = -1, 16
  local done = false
  while not done and -stop <= length do
    local start = math.max(stop - page + 1, -length)
    local chunk = redis.call('LRANGE', key, start, stop)
    for position = #chunk, 1, -1 do
      if sequence_of(chunk[position]) <= since then
        done = true
        break
      end
      newest_first[#newest_first + 1] = chunk[position]
    end
    stop = start - 1
    page = page * 2
  end

  local entries = {}
  for position = #newest_first, 1, -1 do
    entries[#entries + 1] = newest_first[position]
  end
  return entries
end

-- A stream may open before anything is published on a new run, so this
-- script checks the run too.
local epoch = current_epoch(log, recent, floors, sizes, retired, prefix, ARGV[1])
if not epoch then
  return redis.error_reply('anubis realtime: INFO server reported no run_id')
end
local head = tonumber(redis.call('HGET', log, 'sequence'))
local watermark = tonumber(redis.call('HGET', log, 'watermark'))

-- Whether every event on a channel after the cursor is still kept: the
-- cursor is one this log minted, and nothing after it was let go, by the
-- channel's capacity or, for a channel with no buffer, by the budget.
local function vouches_for(key, channel, cursor_epoch, cursor)
  if cursor_epoch ~= epoch or cursor > head then
    return false
  end
  local floor = watermark
  if redis.call('EXISTS', key) == 1 then
    floor = tonumber(redis.call('HGET', floors, channel) or '0')
  end
  return floor <= cursor
end

local openings = {}
for first = 3, #ARGV, 5 do
  local channel, start = ARGV[first], ARGV[first + 1]
  local cursor_epoch, cursor = ARGV[first + 2], tonumber(ARGV[first + 3])
  local backfill = ARGV[first + 4] == '1'
  local key = buffer_key(prefix, epoch, channel)

  if start == 'after' and vouches_for(key, channel, cursor_epoch, cursor) then
    -- An exact resume: what came after the cursor, and nothing to clear.
    openings[#openings + 1] = { 0, entries_after(key, cursor) }
  elseif backfill then
    -- Everything kept, after a reset unless this is the stream's first
    -- connection, which has nothing before it to clear.
    local reset = 1
    if start == 'fresh' then
      reset = 0
    end
    openings[#openings + 1] = { reset, redis.call('LRANGE', key, 0, -1) }
  else
    openings[#openings + 1] = { 1, {} }
  end
end

return { { epoch, head }, openings }
