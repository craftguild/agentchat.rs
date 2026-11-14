import {test,expect} from 'vitest';
import {checkMessageReceipts} from '../../src/graphql/receipt-validation.ts';
import {fixtures} from '../../scripts/dev-project.mjs';

test('generated receipt wire accepts six controlled outcomes and historical readers without advancing delivery',()=>{
  const seed=fixtures();
  for(const row of seed.DevReceipt){
    const message=seed.Message.find(message=>message.id===row.messageId)!;
    const detail={message,receipts:{items:[row.receipt],nextCursor:null,scanCursor:'2',head:'2'}};
    expect(()=>checkMessageReceipts({message:detail},{messageId:message.id})).not.toThrow();
  }
});
test('malformed state, missing required history, wrong message and oversized reader sets fail before publication',()=>{
  const seed=fixtures(),message=seed.Message[0]!,receipt=seed.DevReceipt[0]!.receipt;
  const payload=()=>({message:{message,receipts:{items:[structuredClone(receipt)],nextCursor:null,scanCursor:'1',head:'1'}}});
  const unknown=payload();unknown.message.receipts.items[0]!.delivery!.state='ACKNOWLEDGED';
  const missing=payload();delete (missing.message.receipts.items[0]!.delivery as any).queuedAt;
  const omitted=payload();delete (omitted.message.receipts.items[0]!.delivery as any).deliveredAt;
  const readers=payload();readers.message.receipts.items[0]!.reads=Array.from({length:3},()=>({kind:'EXPLICIT',at:message.createdAt}));
  for(const data of [unknown,missing,omitted,readers])expect(()=>checkMessageReceipts(data,{messageId:message.id})).toThrow();
  expect(()=>checkMessageReceipts(payload(),{messageId:'another-message'})).toThrow();
});
