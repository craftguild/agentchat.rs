import assert from 'node:assert/strict';
import {test} from 'node:test';
import {fixtures} from '../../scripts/dev-project.mjs';
import {startDev} from './helper.ts';
import {createSdk} from '../../src/graphql/sdk.ts';
import {createViewerClient} from '../../src/graphql/client.ts';

test('controlled Mock receipt cases match generated SDK history and independent reader pages',{timeout:90000},async()=>{
  const dev=await startDev(),original=globalThis.fetch;
  try{
    globalThis.fetch=(url,options)=>original(new URL(url as string,dev.origin),options);
    const sdk=createSdk(createViewerClient()),signal=AbortSignal.timeout(60000),seed=fixtures();
    const expected=['PENDING','PROCESSING','DELIVERED','FAILED_TIMEOUT','FAILED_LEFT','DELIVERED_LATE','FAILED_LEFT','DELIVERED_LATE'];
    for(const [index,root] of seed.Message.filter(message=>message.rootId===null).slice(0,8).entries()){
      const first=await sdk.MessageReceipts({messageId:root.id,after:null,limit:1},{signal});
      assert.equal(first.message.message.body,root.body);
      const receipt=first.message.receipts.items[0]!,delivery=receipt.delivery!;
      assert.equal(delivery.state,expected[index]);
      assert.equal(delivery.queuedAt,first.message.message.createdAt);
      for(const [field,value] of Object.entries(seed.DevReceipt[index*2]!.receipt.delivery!))
        assert.equal(delivery[field as keyof typeof delivery],value);
      assert.equal(first.message.receipts.nextCursor,'1');
      const second=await sdk.MessageReceipts({messageId:root.id,after:first.message.receipts.nextCursor,limit:1},{signal});
      assert.equal(second.message.receipts.nextCursor,null);
      const reader=second.message.receipts.items[0]!;
      assert.equal(reader.recipient.kind,'USER');assert.equal(reader.delivery,null);
      assert.equal(reader.reads[0]!.kind,'EXPLICIT');
      assert(Date.parse(reader.reads[0]!.at)>=Date.parse(first.message.message.createdAt));
      if(delivery.deliveredAt===null)assert.deepEqual(receipt.reads,[]);
      else assert.deepEqual(receipt.reads.map(({kind,at})=>({kind,at})),[{kind:'AUTOMATIC',at:delivery.deliveredAt}]);
    }
    const pending=seed.DevReceipt[0]!.receipt.delivery!,processing=seed.DevReceipt[2]!.receipt.delivery!;
    assert.equal(pending.processingStartedAt,null);assert.equal(processing.deadlineAt,'2026-10-24T11:00:00.000Z');
    const late=seed.DevReceipt[14]!.receipt.delivery!;assert.equal(late.timedOutAt,late.deliveredAt);
    const head=await sdk.Project({}, {signal});assert.equal(head.project.checkpoint.seq,seed.Project[0]!.checkpoint.seq);
  }finally{globalThis.fetch=original;await dev.close();}
});
