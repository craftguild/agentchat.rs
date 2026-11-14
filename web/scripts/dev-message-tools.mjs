/** Inlined Mock authoring helpers; no private SDK modules or generated edits. */
export const trimBody = `
-- Native string/table calls charge their complete inputs. Split in small chunks
-- and use local byte arrays, avoiding quadratic work for long or escaped bodies.
local function byteList(input: string): {string}
    local bytes: {string} = {}
    for first = 1, #input, 32 do
        local chunk = string.sub(input, first, first + 31)
        for offset = 1, #chunk do bytes[#bytes + 1] = string.sub(chunk, offset, offset) end
    end
    return bytes
end
local function trimBody(input: string): string
    local whitespace: {string} = {"\\9", "\\10", "\\11", "\\12", "\\13", " ", "\u0085", "\u00a0", "\u1680", "\u2000", "\u2001", "\u2002", "\u2003", "\u2004", "\u2005", "\u2006", "\u2007", "\u2008", "\u2009", "\u200a", "\u2028", "\u2029", "\u202f", "\u205f", "\u3000"}
    local bytes = byteList(input)
    local function spaceAt(index: number, backwards: boolean): number
        for _, space in whitespace do
            local piece = ""
            local start = if backwards then index - #space + 1 else index
            for offset = 0, #space - 1 do piece ..= bytes[start + offset] or "" end
            if piece == space then return #space end
        end
        return 0
    end
    local first, last = 1, #input
    while first <= last do
        local size = spaceAt(first, false)
        if size == 0 then break end
        first += size
    end
    while first <= last do
        local size = spaceAt(last, true)
        if size == 0 then break end
        last -= size
    end
    return string.sub(input, first, last)
end
`;

export const messageTools = `
local function escapeText(input: string): string
    local chunks: {string} = {}
    for _, c in byteList(input) do
        chunks[#chunks + 1] = if c == "&" then "&amp;" elseif c == "<" then "&lt;" elseif c == ">" then "&gt;" else c
    end
    return table.concat(chunks)
end
local function letter(c: string): boolean
    return c ~= "" and ((c >= "a" and c <= "z") or (c >= "A" and c <= "Z"))
end
local function word(c: string): boolean
    return letter(c) or (c ~= "" and c >= "0" and c <= "9")
end
-- Plain-text fixture decoration, not a CommonMark parser. Escape all input;
-- skip escaped @, emails/URLs and backtick code; only generated spans are trusted.
local function renderBody(body: string): (string, {[string]: boolean})
    local prefix, suffix = '<div class="markdown-fallback">', '</div>'
    local allowance = 4096 - #prefix - #suffix - #escapeText(body)
    local html: {string} = {}
    local handles: {[string]: boolean} = {}
    local bytes = byteList(body)
    local index, code = 1, 0
    while index <= #body do
        local c = bytes[index]
        local previous = bytes[index - 1] or ""
        if c == "\u0060" then
            local ending = index
            while bytes[ending] == "\u0060" do ending += 1 end
            local count = ending - index
            if code == 0 then code = count elseif code == count then code = 0 end
            html[#html + 1] = string.sub(body, index, ending - 1); index = ending
        else
            local slashes, position = 0, index - 1
            while bytes[position] == "\\\\" and position > 0 do slashes += 1; position -= 1 end
            local boundary = previous == "" or (not word(previous) and previous < "\\128" and previous ~= "_" and previous ~= "-" and previous ~= "/" and previous ~= "." and previous ~= ":" and previous ~= "@" and previous ~= "%")
            if c == "@" and code == 0 and boundary and slashes % 2 == 0 and letter(bytes[index + 1] or "") then
                local ending = index + 1
                while word(bytes[ending] or "") or bytes[ending] == "-" do ending += 1 end
                local token = string.sub(body, index, ending - 1)
                local handle = string.lower(string.sub(token, 2))
                handles[handle] = true
                local span = '<span class="mention" data-mention="' .. handle .. '">' .. token .. '</span>'
                if #span - #token <= allowance then html[#html + 1] = span; allowance -= #span - #token else html[#html + 1] = token end
                index = ending
            else
                html[#html + 1] = if c == "&" then "&amp;" elseif c == "<" then "&lt;" elseif c == ">" then "&gt;" else c
                index += 1
            end
        end
    end
    return prefix .. table.concat(html) .. suffix, handles
end
local function appendMessage(seq: number, channel: ChigiriChannelRecord, authorId: string, authorKind: ChigiriActorKindValue, authorHandle: string, rootId: string, body: string)
    local suffix = string.sub("000000000000" .. tostring(30000 + seq), -12)
    local id = "00000000-0000-0000-0000-" .. suffix
    local html = renderBody(body)
    local replies = "0"
    if rootId ~= "" then
        local root = ctx.models.Message:find({where = {field = "id", op = "eq", value = rootId}, limit = 1})[1]
        replies = tostring(assert(tonumber(root.replyCount)) + 1)
        ctx.models.Message:update({where = {field = "id", op = "eq", value = rootId}}, {replyCount = replies})
    end
    local message = ctx.models.Message:create({id = id, channelId = channel.id, channel = channel.name, author = {id = authorId, kind = authorKind, handle = authorHandle},
        rootId = if rootId == "" then ctx.null else rootId, threadId = if rootId == "" then id else rootId,
        creationSeq = tostring(seq), createdAt = "2026-10-24T12:00:00.000Z", body = body, bodyHtml = html, renderFallback = true, replyCount = replies})
    ctx.models.DevMessageIndex:create({id = id, channel = channel.name, rootId = if rootId == "" then ctx.null else rootId, seq = seq})
    ctx.models.Change:create({eventId = "00000000-0000-0000-0001-" .. suffix, seq = tostring(seq + 1), kind = "MESSAGE_CREATED", channel = ctx.null, message = message})
    return id
end
local function postMessages(channel: ChigiriChannelRecord, authorId: string, authorKind: ChigiriActorKindValue, authorHandle: string, rootId: string, body: string): (string, ChigiriChangeBatchRecord)
    local _, handles = renderBody(body)
    local targets: {ChigiriAgentRecord} = {}
    for _, agent in ctx.models.Agent:find({limit = 50}) do
        if agent.joined and agent.id ~= authorId and inChannel(channel.name, agent.handle)
            and (handles[agent.handle] or handles.all or handles.here or handles.channel) then table.insert(targets, agent) end
    end
    local first = assert(tonumber(head.seq)) + 1
    local last = first + 2 * #targets + 1
    if eventCount + #targets + 1 > 160 then return ctx.fail({code = "DEV_LIMIT", message = "Restart development to reset its bounded fixture."}) end
    local id = appendMessage(first, channel, authorId, authorKind, authorHandle, rootId, body)
    for offset, agent in targets do
        -- One deterministic reply per recipient; generated echoes never recurse.
        appendMessage(first + 2 * offset, channel, agent.id, "AGENT", agent.handle, if rootId == "" then id else rootId, "Echo: " .. body)
    end
    local items: {ChigiriChangeRecord} = {}
    for seq = first + 1, last, 2 do
        table.insert(items, ctx.models.Change:find({where = {field = "seq", op = "eq", value = tostring(seq)}, limit = 1})[1])
    end
    ctx.models.DevEventBudget:update({where = {field = "id", op = "eq", value = "budget"}}, {events = eventCount + #targets + 1})
    local event = items[#items]
    local nextHead: ChigiriCheckpointRecord = {projectId = projectId, seq = event.seq, eventId = event.eventId}
    ctx.models.Project:update({where = {field = "id", op = "eq", value = projectId}}, {checkpoint = nextHead})
    return id, {after = head, scan = nextHead, head = nextHead, items = items, hasMore = false}
end
`;
