-- What the replay log holds for some channels, read at one instant.
--
-- The counterpart of `anubis::realtime::replay::MemoryLog::snapshot`. One
-- script reads the head, the watermark, and every buffer together, so nothing
-- published while it runs can land between two of them.
--
-- KEYS: 1 the log hash, 2 the recency sorted set, 3 the floors hash, 4 the
--       sizes hash, 5.. one buffer list per channel.
-- ARGV: 1 a candidate epoch, used only when the log starts a new one, 2 the
--       prefix of every buffer key, then one pair per buffer key: the
--       channel's name, and the sequence to replay after, or -1 to replay
--       nothing.
--
-- Returns { { epoch, head, watermark }, { { exists, floor, { entry, ... } }, ... } },
-- the views in the order the channels were named. An entry is
-- "<sequence> <frame>".

local log, recent, floors, sizes = KEYS[1], KEYS[2], KEYS[3], KEYS[4]
local buffer_prefix = ARGV[2]

local function sequence_of(entry)
  return tonumber(string.match(entry, '^%d+'))
end

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

-- A Redis that restarted or failed over starts a new epoch and lets go of
-- everything kept, exactly as publish.lua explains; a stream may open before
-- anything is published on the new run, so this script checks too.
local run_id = string.match(redis.call('INFO', 'server'), 'run_id:(%x+)')
if not run_id then
  return redis.error_reply('anubis realtime: INFO server reported no run_id')
end
if redis.call('HGET', log, 'run_id') ~= run_id then
  for _, kept in ipairs(redis.call('ZRANGE', recent, 0, -1)) do
    redis.call('DEL', buffer_prefix .. kept)
  end
  redis.call('DEL', log, recent, floors, sizes)
  redis.call('HSET', log, 'epoch', ARGV[1], 'run_id', run_id,
    'sequence', '0', 'bytes', '0', 'watermark', '0')
end

local epoch = redis.call('HGET', log, 'epoch')
local head = tonumber(redis.call('HGET', log, 'sequence'))
local watermark = tonumber(redis.call('HGET', log, 'watermark'))

local views = {}
for index = 5, #KEYS do
  local pair = (index - 5) * 2
  local channel = ARGV[pair + 3]
  local since = tonumber(ARGV[pair + 4])
  local exists = redis.call('EXISTS', KEYS[index])
  local floor = tonumber(redis.call('HGET', floors, channel) or '0')

  local entries = {}
  if exists == 1 and since >= 0 then
    entries = entries_after(KEYS[index], since)
  end

  views[#views + 1] = { exists, floor, entries }
end

return { { epoch, head, watermark }, views }
