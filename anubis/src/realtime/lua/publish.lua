-- One realtime publish, atomically: mint the next id, keep the event for
-- replay, trim its channel to capacity, keep the deployment within its byte
-- budget, and fan the event out. One script is what makes the pub/sub order
-- the sequence order, and what keeps the floors and the watermark true.
--
-- The arithmetic is `anubis::realtime::replay::MemoryLog::append`'s, kept in
-- step with it; `docs/realtime.md` states the rules both follow.
--
-- KEYS: 1 the log hash (epoch, run_id, sequence, bytes, watermark), 2 the
--       recency sorted set (channel by newest sequence), 3 the floors hash,
--       4 the sizes hash, 5 this channel's buffer list.
-- ARGV: 1 a candidate epoch, used only when the log starts a new one, 2 the
--       channel, 3 the rendered frame, 4 the channel's capacity, 5 the byte
--       budget, 6 the pub/sub channel, 7 the prefix of every buffer key, 8 what
--       one entry costs beyond its text, 9 what one buffer costs before it
--       holds anything.
--
-- Returns { epoch, sequence }.

local log, recent, floors, sizes, buffer = KEYS[1], KEYS[2], KEYS[3], KEYS[4], KEYS[5]
local channel, data = ARGV[2], ARGV[3]
local capacity, budget = tonumber(ARGV[4]), tonumber(ARGV[5])
local topic, buffer_prefix = ARGV[6], ARGV[7]
local entry_overhead, buffer_overhead = tonumber(ARGV[8]), tonumber(ARGV[9])

-- Numbers cross into Redis as text, and Lua renders a large one in exponent
-- form unless it is formatted as an integer.
local function integer(value)
  return string.format('%d', value)
end

-- An entry is "<sequence> <frame>"; a frame is JSON on one line.
local function sequence_of(entry)
  return tonumber(string.match(entry, '^%d+'))
end

local function cost_of(entry)
  return #entry - string.find(entry, ' ', 1, true) + entry_overhead
end

-- The log belongs to one run of one Redis server. A server that restarted,
-- from an older snapshot or append-only file than the last publish, or a
-- replica promoted in its place, may hold a sequence behind ids clients
-- already have, and would mint those ids again for other events. Its run_id
-- is new in every one of those cases, so a new run_id starts a new epoch and
-- lets go of everything kept: no cursor from before can be vouched for, and
-- every client resuming on one opens with a reset. snapshot.lua does the same.
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
local sequence = redis.call('HINCRBY', log, 'sequence', 1)
local watermark = tonumber(redis.call('HGET', log, 'watermark'))

if redis.call('EXISTS', buffer) == 0 then
  -- A buffer born now cannot vouch for anything let go before it, so it
  -- starts at the watermark rather than at zero.
  redis.call('HSET', floors, channel, integer(watermark))
  redis.call('HSET', sizes, channel, integer(buffer_overhead))
  redis.call('HINCRBY', log, 'bytes', integer(buffer_overhead))
end

local entry = integer(sequence) .. ' ' .. data
local cost = cost_of(entry)
redis.call('RPUSH', buffer, entry)
redis.call('HINCRBY', sizes, channel, integer(cost))
redis.call('HINCRBY', log, 'bytes', integer(cost))
redis.call('ZADD', recent, integer(sequence), channel)

local excess = redis.call('LLEN', buffer) - capacity
while excess > 0 do
  local dropped = redis.call('LPOP', buffer)
  local freed = cost_of(dropped)
  redis.call('HSET', floors, channel, integer(sequence_of(dropped)))
  redis.call('HINCRBY', sizes, channel, integer(-freed))
  redis.call('HINCRBY', log, 'bytes', integer(-freed))
  excess = excess - 1
end

-- Whole channels go, least recently published first, until the log fits.
while tonumber(redis.call('HGET', log, 'bytes')) > budget do
  local oldest = redis.call('ZPOPMIN', recent)
  if #oldest == 0 then
    break
  end
  local victim, newest = oldest[1], tonumber(oldest[2])
  local size = tonumber(redis.call('HGET', sizes, victim) or '0')
  redis.call('DEL', buffer_prefix .. victim)
  redis.call('HDEL', sizes, victim)
  redis.call('HDEL', floors, victim)
  redis.call('HINCRBY', log, 'bytes', integer(-size))
  if newest > watermark then
    watermark = newest
    redis.call('HSET', log, 'watermark', integer(watermark))
  end
end

redis.call('PUBLISH', topic, epoch .. ' ' .. entry)

return { epoch, sequence }
