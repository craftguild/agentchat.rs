/** User-action simulation in the temporary Mock; production uses shared Rust operations. */
import { trimBody, messageTools } from './dev-message-tools.mjs';
export const userCommon = trimBody + `
local function fail(code: string, message: string): never
  return ctx.fail({code = code, message = message})
end
local operation = "OPERATION"
local channelName = CHANNEL_VALUE
local rootId = ROOT_VALUE
local body = BODY_VALUE
local signature = channelName .. "\\0" .. rootId .. "\\0" .. body
local previous = ctx.models.DevRequest:find({where = {field = "id", op = "eq", value = args.requestId}})[1]
if previous ~= nil then
  if previous.operation ~= operation or previous.arguments ~= signature then return fail("idempotency_conflict", "Request identifies another action.") end
  REPLAY
end
if eventCount >= 160 or #body > 768 then return fail("DEV_LIMIT", "Restart development to reset its bounded fixture.") end
body = trimBody(body)
local function joinUser(channel: string)
  if ctx.models.Actor:find({where = {field = "id", op = "eq", value = "00000000-0000-0000-0000-000000000002"}})[1] == nil then
    ctx.models.Actor:create({id = "00000000-0000-0000-0000-000000000002", kind = "USER", handle = "user", joined = true, host = ctx.null, chatId = ctx.null})
  end
  if ctx.models.DevMembership:find({where = {field = "channel", op = "eq", value = channel}})[1] == nil then
    ctx.models.DevMembership:create({id = "user-" .. channel, channel = channel})
  end
end
local function save(id: string)
  ctx.models.DevRequest:create({id = args.requestId, operation = operation, arguments = signature, resultId = id})
end
local seq = tostring(assert(tonumber(head.seq)) + 1)
local suffix = string.sub("000000000000" .. tostring(30000 + assert(tonumber(seq))), -12)
local id = "00000000-0000-0000-0000-" .. suffix
local eventId = "00000000-0000-0000-0001-" .. suffix
`;

export const createChannel = userCommon.replace('CHANNEL_VALUE','string.lower(args.name)').replace('ROOT_VALUE','""').replace('BODY_VALUE','""').replace('OPERATION','create').replace('REPLAY', `
  local old = ctx.models.Channel:find({where = {field = "id", op = "eq", value = previous.resultId}})[1]
  return {channelId = old.id, name = old.name}
`) + `
local valid = #channelName > 0 and #channelName <= 64
local previousDash = false
for index = 1, #channelName do
  local c = string.sub(channelName, index, index)
  local letter = c >= "a" and c <= "z"
  local digit = c >= "0" and c <= "9"
  local dash = c == "-"
  if (index == 1 and not letter) or (not letter and not digit and not dash) or (dash and previousDash) then valid = false end
  previousDash = dash
end
if not valid or previousDash then return fail("invalid_name", "Use a valid channel slug.") end
local old = ctx.models.Channel:find({where = {field = "name", op = "eq", value = channelName}})[1]
if old ~= nil then
  joinUser(channelName); save(old.id)
  return {channelId = old.id, name = old.name}
end
joinUser(channelName)
local channel = ctx.models.Channel:create({id = id, name = channelName, creationSeq = seq})
local event = ctx.models.Change:create({eventId = eventId, seq = seq, kind = "CHANNEL_CREATED", channel = channel, message = ctx.null})
ctx.models.DevEventBudget:update({where = {field = "id", op = "eq", value = "budget"}}, {events = eventCount + 1})
save(id)
  local nextHead: ChigiriCheckpointRecord = {projectId = projectId, seq = event.seq, eventId = event.eventId}
  ctx.models.Project:update({where = {field = "id", op = "eq", value = projectId}}, {checkpoint = nextHead})
  local batch: ChigiriChangeBatchRecord = {after = head, scan = nextHead, head = nextHead, items = {event}, hasMore = false}
  PUBSUB_PUBLISH({topic = "viewer-dev", payload = batch})

return {channelId = id, name = channelName}
`;

const message = userCommon.replace('OPERATION','MESSAGE_OPERATION').replace('REPLAY','return {messageId = previous.resultId, warnings = {}}') + messageTools + `
local root = if rootId ~= "" then ctx.models.Message:find({where = {field = "id", op = "eq", value = rootId}})[1] else nil
if rootId ~= "" then
  if root == nil or (root.rootId ~= nil and root.rootId ~= ctx.null) then return fail("invalid_thread", "Reply to a thread root.") end
  channelName = root.channel
end
local channel = ctx.models.Channel:find({where = {field = "name", op = "eq", value = channelName}})[1]
if channel == nil then return fail("not_found", "Channel not found.") end
if #body == 0 then return fail("invalid_argument", "Enter a message.") end
joinUser(channelName)
local postedId, batch = postMessages(channel, "00000000-0000-0000-0000-000000000002", "USER", "user", rootId, body)
save(postedId)
  PUBSUB_PUBLISH({topic = "viewer-dev", payload = batch})

return {messageId = postedId, warnings = {}}
`;
export const postMessage = message.replace('CHANNEL_VALUE','args.channel').replace('ROOT_VALUE','""').replace('BODY_VALUE','args.body').replace('MESSAGE_OPERATION','post');
export const replyMessage = message.replace('CHANNEL_VALUE','""').replace('ROOT_VALUE','args.rootId').replace('BODY_VALUE','args.body').replace('MESSAGE_OPERATION','reply');
