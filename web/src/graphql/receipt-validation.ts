import type { MessageReceiptsQuery } from './catalog.generated.ts';
import { decimal, record, text } from '../validate.ts';
import { WireError } from '../types.ts';

type Delivery=NonNullable<MessageReceiptsQuery['message']['receipts']['items'][number]['delivery']>;
const states={PENDING:true,PROCESSING:true,DELIVERED:true,FAILED_TIMEOUT:true,FAILED_LEFT:true,DELIVERED_LATE:true} satisfies Record<Delivery['state'],boolean>;
const invalid=()=>{throw new WireError('Invalid recipient receipt.','invalid_frame',true);};
const instant=(value:unknown)=>{const encoded=text(value,64);if(!Number.isFinite(Date.parse(encoded)))invalid();};
const identity=(value:unknown)=>{const actor=record(value);text(actor.id,36);text(actor.handle,64);if(actor.kind!=='AGENT'&&actor.kind!=='USER')invalid();};

/** Structural wire bounds only; Core owns state transitions and timestamp policy. */
export function checkMessageReceipts(data:Record<string,unknown>,variables:Record<string,unknown>):void {
  const detail=record(data.message),message=record(detail.message),page=record(detail.receipts);
  if(message.id!==variables.messageId)invalid();
  text(message.id,36);text(message.channelId,36);text(message.channel,64);identity(message.author);
  if(message.rootId!==null)text(message.rootId,36);
  text(message.threadId,36);decimal(message.creationSeq);instant(message.createdAt);text(message.body,65_536);
  if(!Array.isArray(page.items)||page.items.length>200)invalid();
  for(const value of page.items as unknown[]){
    const row=record(value);identity(row.recipient);
    if(row.delivery!==null){
      const delivery=record(row.delivery);text(delivery.deliveryId,36);
      if(typeof delivery.state!=='string'||!Object.hasOwn(states,delivery.state))invalid();
      instant(delivery.queuedAt);
      for(const field of ['processingStartedAt','deadlineAt','timedOutAt','deliveredAt','leftAt'])
        if(delivery[field]!==null)instant(delivery[field]);
    }
    if(!Array.isArray(row.reads)||row.reads.length>2)invalid();
    for(const value of row.reads as unknown[]){const read=record(value);if(read.kind!=='AUTOMATIC'&&read.kind!=='EXPLICIT')invalid();instant(read.at);}
  }
  if(page.nextCursor!==null)decimal(page.nextCursor);decimal(page.scanCursor);decimal(page.head);
}
