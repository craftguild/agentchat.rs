/** Authoring fragments: Chigiri Mock does not support shared Luau modules. */
import { trimBody, messageTools } from './dev-message-tools.mjs';

export const changes = `if args.after.projectId ~= projectId then
    return ctx.fail({code = "INVALID_CURSOR", message = "Reload the development snapshot."})
end
local after = checkpoint(args.after.seq)
local through = head
if type(args.through) == "table" then through = checkpoint(args.through.seq) end
local items: {ChigiriChangeRecord} = {}
local limit = if type(args.limit) == "number" then math.min(20, math.max(1, args.limit)) else 20
local bytes = 0
for seq = assert(tonumber(after.seq)) + 1, assert(tonumber(through.seq)) do
    local event = ctx.models.Change:find({where = {field = "seq", op = "eq", value = tostring(seq)}, limit = 1})[1]
    if event ~= nil then
    local size = 256
    if event.kind == "MESSAGE_CREATED" then
        local message = event.message :: ChigiriMessageRecord
        size += #message.body + #message.bodyHtml
    end
    if #items >= limit or (#items > 0 and bytes + size > 16384) then break end
    items[#items + 1] = event; bytes += size
    end
end
local more = #items > 0 and assert(tonumber(items[#items].seq)) < assert(tonumber(through.seq))
local scan = if more then checkpoint(items[#items].seq) else through
return {after = after, scan = scan, head = through, items = items, hasMore = more}
`;

export const common = `-- Shared authoring fragment, inserted into generated resolver bodies at startup.
-- Chigiri Mock has no project-local require; generated project files are temporary.
local projectId = "00000000-0000-0000-0000-000000000001"
-- Read the lightweight head, not every retained message/event, on each Query.
local head = ctx.models.Project:find({limit = 1})[1].checkpoint
local eventCount = ctx.models.DevEventBudget:find({limit = 1})[1].events
local function checkpoint(seq: string): ChigiriCheckpointRecord
    if seq == "0" then return {projectId = projectId, seq = "0", eventId = ctx.null} end
    local event = ctx.models.Change:find({where = {field = "seq", op = "eq", value = seq}, limit = 1})[1]
    if event == nil then return ctx.fail({code = "INVALID_CURSOR", message = "Reload the development snapshot."}) end
    return {projectId = projectId, seq = seq, eventId = event.eventId}
end
local function inChannel(channel: string, handle: string): boolean
    return channel == "dev" or (channel == "design" and (handle == "violet-fox" or handle == "blue-otter"))
        or (channel == "random" and handle == "green-wren")
end
`;

export const post = trimBody + messageTools + `-- This mutation exists only in the temporary Mock schema, never in product SDL.
if #args.body > 768 then
    return ctx.fail({code = "DEV_LIMIT", message = "Restart dev to reset fixtures; new bodies are limited to 768 bytes."})
end
local body = trimBody(args.body)
if #body == 0 then return ctx.fail({code = "invalid_argument", message = "Enter a message."}) end
local channel = ctx.models.Channel:find({where = {field = "name", op = "eq", value = args.channel}})[1]
if channel == nil then return ctx.fail({code = "CHANNEL_NOT_FOUND", message = "Use dev, design or random."}) end
local _, batch = postMessages(channel, "00000000-0000-0000-0000-000000000003", "AGENT", "dev-bot", "", body)
PUBSUB_PUBLISH({topic = "viewer-dev", payload = batch})
return batch
`;

export const window = `local snapshot = head
if type(args.snapshot) == "table" then snapshot = checkpoint(args.snapshot.seq) end
local before = if type(args.before) == "string" then tonumber(args.before) or 0 else assert(tonumber(snapshot.seq)) + 1
local limit = if type(args.limit) == "number" then math.min(20, math.max(1, args.limit)) else 20
-- Select small metadata first; never materialize every retained body just to page.
local indices = ctx.models.DevMessageIndex:find({where = {all = {
    {field = "channel", op = "eq", value = args.channel}, ROOT_CONDITION,
    {field = "seq", op = "lte", value = assert(tonumber(snapshot.seq))}, {field = "seq", op = "lt", value = before}}},
    orderBy = {{field = "seq", direction = "desc"}}, limit = limit + 1})
local descending: {ChigiriMessageRecord} = {}
local bytes = 0
for _, entry in indices do
    local message = ctx.models.Message:find({where = {field = "id", op = "eq", value = entry.id}, limit = 1})[1]
    local size = #message.body + #message.bodyHtml + 256
    if #descending >= limit or (#descending > 0 and bytes + size > 16384) then break end
    descending[#descending + 1] = message; bytes += size
end
local items: {ChigiriMessageRecord} = {}
for index = #descending, 1, -1 do items[#items + 1] = descending[index] end
local hasOlder = #indices > #items
return {snapshot = snapshot, items = items, nextBefore = if hasOlder then items[1].creationSeq else ctx.null}
`;
