/** Fixed contract examples, not a second implementation of Core delivery policy. */
export function receiptScenarios() {
  const queuedAt='2026-10-24T09:00:00.000Z',processingStartedAt='2026-10-24T10:00:00.000Z';
  const deadlineAt='2026-10-24T11:00:00.000Z',timedOutAt='2026-10-24T11:00:01.000Z';
  const pending={queuedAt,processingStartedAt:null,deadlineAt:null,timedOutAt:null,deliveredAt:null,leftAt:null};
  const processing={...pending,processingStartedAt,deadlineAt};
  const timeout={...processing,timedOutAt};
  return [
    {state:'PENDING',...pending},
    {state:'PROCESSING',...processing},
    // Exact deadline acceptance remains timely; pending time did not consume it.
    {state:'DELIVERED',...processing,deliveredAt:deadlineAt},
    {state:'FAILED_TIMEOUT',...timeout},
    {state:'FAILED_LEFT',...pending,leftAt:processingStartedAt},
    {state:'DELIVERED_LATE',...timeout,deliveredAt:'2026-10-24T11:00:02.000Z'},
    {state:'FAILED_LEFT',...processing,leftAt:'2026-10-24T10:00:01.000Z'},
    // Delayed maintenance discovers expiry and acceptance in one commit.
    {state:'DELIVERED_LATE',...timeout,deliveredAt:timedOutAt},
  ];
}

export function receiptFixtures(messages,agents,id) {
  return messages.filter(message=>message.rootId===null).slice(0,8).flatMap((message,index)=>{
    const scenario={...receiptScenarios()[index],queuedAt:message.createdAt},agent=agents.find(agent=>agent.id!==message.author.id);
    const recipient={id:agent.id,kind:agent.kind,handle:agent.handle};
    const reads=scenario.deliveredAt===null?[]:[{kind:'AUTOMATIC',at:scenario.deliveredAt}];
    return [
      {id:id(30000+index*2),messageId:message.id,sequence:1,receipt:{recipient,
        delivery:{deliveryId:id(40000+index),...scenario},reads}},
      {id:id(30001+index*2),messageId:message.id,sequence:2,receipt:{
        recipient:{id:id(2),kind:'USER',handle:'user'},delivery:null,
        reads:[{kind:'EXPLICIT',at:'2026-10-24T10:00:00.000Z'}]}},
    ];
  });
}

/** Paged canonical reads preserve null delivery for historical readers. */
export const messageDetail=`local message = ctx.models.Message:find({where = {field = "id", op = "eq", value = args.messageId}, limit = 1})[1]
if message == nil then return ctx.fail({code = "NOT_FOUND", message = "Development message not found."}) end
local rows = ctx.models.DevReceipt:find({where = {field = "messageId", op = "eq", value = args.messageId},
    orderBy = {{field = "sequence", direction = "asc"}}, limit = 20})
local after = if type(args.after) == "string" then tonumber(args.after) or 0 else 0
local limit = if type(args.limit) == "number" then math.min(20, math.max(1, args.limit)) else 20
local items: {ChigiriRecipientReceiptRecord} = {}
local last = after
local more = false
local receiptHead = if #rows > 0 then rows[#rows].sequence else 0
for _, row in rows do
    if row.sequence > after then
        if #items >= limit then more = true; break end
        items[#items + 1] = row.receipt; last = row.sequence
    end
end
return {message = {id = message.id, channelId = message.channelId, channel = message.channel,
    author = message.author, rootId = message.rootId, threadId = message.threadId,
    creationSeq = message.creationSeq, createdAt = message.createdAt, body = message.body},
    receipts = {items = items, nextCursor = if more then tostring(last) else ctx.null,
        scanCursor = tostring(if more then last else math.max(receiptHead, after)), head = tostring(receiptHead)}}
`;
