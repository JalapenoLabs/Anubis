-- What publish.lua and snapshot.lua share, sent at the head of each.
--
-- Every key lives under the hash tag {anubis:realtime}, so all of them share
-- one slot. The log hash, the recency set, the floors and sizes hashes, and
-- the list of retired epochs have fixed names, and the scripts declare them.
-- A channel's buffer is named after the epoch it was kept under,
-- "<prefix>buffer:<epoch>:<channel>", and a retired epoch's recency set is
-- "<prefix>retired:<epoch>". Neither can be named before a script has read
-- the epoch, so neither is declared; Redis and Redis Cluster allow that
-- within the declared keys' slot, and docs/realtime.md says what other
-- servers need.

-- An entry is "<sequence> <frame>"; a frame is JSON on one line.
local function sequence_of(entry)
  return tonumber(string.match(entry, '^%d+'))
end

-- The key holding one channel's buffer under one epoch.
local function buffer_key(prefix, epoch, channel)
  return prefix .. 'buffer:' .. epoch .. ':' .. channel
end

-- The epoch the log is on, starting a new one when Redis is on a new run.
--
-- The log belongs to one run of one Redis server. A server that restarted,
-- from an older snapshot or append-only file than the last publish, or a
-- replica promoted in its place, may hold a sequence behind ids clients
-- already have, and would mint those ids again for other events. Its run_id
-- is new in every one of those cases, so a new run_id starts a new epoch: no
-- cursor from before can be vouched for, and every client resuming on one
-- opens with a reset.
--
-- Starting an epoch costs the same however much the last one kept. Its
-- buffers are named after it, so the new epoch never sees them; its recency
-- set is set aside under its name, for publish.lua to let go of a bounded
-- number of its buffers at a time; and its floors and sizes are unlinked,
-- which Redis frees off its main thread.
--
-- Returns nil on a server whose INFO names no run_id.
local function current_epoch(log, recent, floors, sizes, retired, prefix, candidate)
  local run_id = string.match(redis.call('INFO', 'server'), 'run_id:(%x+)')
  if not run_id then
    return nil
  end
  if redis.call('HGET', log, 'run_id') == run_id then
    return redis.call('HGET', log, 'epoch')
  end

  local previous = redis.call('HGET', log, 'epoch')
  if previous and redis.call('EXISTS', recent) == 1 then
    redis.call('RENAME', recent, prefix .. 'retired:' .. previous)
    redis.call('RPUSH', retired, previous)
  end
  redis.call('UNLINK', floors, sizes)
  redis.call('HSET', log, 'epoch', candidate, 'run_id', run_id,
    'sequence', '0', 'bytes', '0', 'watermark', '0')
  return candidate
end
