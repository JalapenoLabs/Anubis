-- What the replay log holds for some channels, read at one instant.
--
-- The counterpart of `anubis::realtime::replay::MemoryLog::snapshot`. One
-- script reads the head, the watermark, and every buffer together, so nothing
-- published while it runs can land between two of them.
--
-- KEYS: 1 the log hash, 2 the floors hash, 3.. one buffer list per channel.
-- ARGV: 1 a candidate epoch, used only if the log has none yet, then one pair
--       per buffer key: the channel's name, and the sequence to replay after,
--       or -1 to replay nothing.
--
-- Returns { { epoch, head, watermark }, { { exists, floor, { entry, ... } }, ... } },
-- the views in the order the channels were named. An entry is
-- "<sequence> <frame>".

local log, floors = KEYS[1], KEYS[2]

local function sequence_of(entry)
  return tonumber(string.match(entry, '^%d+'))
end

-- A deployment that has never published still has an epoch to hand out, so a
-- stream opened on it holds a cursor from its first moment.
redis.call('HSETNX', log, 'epoch', ARGV[1])
local epoch = redis.call('HGET', log, 'epoch')
local head = tonumber(redis.call('HGET', log, 'sequence') or '0')
local watermark = tonumber(redis.call('HGET', log, 'watermark') or '0')

local views = {}
for index = 3, #KEYS do
  local pair = (index - 3) * 2
  local channel = ARGV[pair + 2]
  local since = tonumber(ARGV[pair + 3])
  local exists = redis.call('EXISTS', KEYS[index])
  local floor = tonumber(redis.call('HGET', floors, channel) or '0')

  local entries = {}
  if exists == 1 and since >= 0 then
    -- Entries are in sequence order, so the ones after `since` are the run at
    -- the back; walking from the newest stops at the first one already heard.
    local kept = redis.call('LRANGE', KEYS[index], 0, -1)
    local first = #kept + 1
    while first > 1 and sequence_of(kept[first - 1]) > since do
      first = first - 1
    end
    for position = first, #kept do
      entries[#entries + 1] = kept[position]
    end
  end

  views[#views + 1] = { exists, floor, entries }
end

return { { epoch, head, watermark }, views }
