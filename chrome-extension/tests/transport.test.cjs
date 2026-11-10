const assert = require('assert').strict;
const {deliverPurchase, restorePendingPurchase} = require('../transport.js');

(async () => {
  const payload = {requestID: 'same-request-on-retry'};
  const old = {
    targets: ['http', 'web'],
    payload,
    receipts: {http: 'already-saved'},
    projectLists: {http: [{name: 'Desktop project'}], web: [{name: 'Browser project'}]},
    base: 'https://127.0.0.1:8824',
    tabId: 8,
  };
  const saved = restorePendingPurchase(old, 'web');
  assert.equal(saved.destination, 'http');
  assert.equal(saved.receipt, 'already-saved');
  await deliverPurchase(saved, () => assert.fail('Already saved'), () => assert.fail('Already recorded'));

  const pending = restorePendingPurchase({...old, receipts: {}}, 'web');
  const calls = [];
  let fail = true;
  const send = async attempt => {
    calls.push([attempt.destination, attempt.tabId, attempt.payload.requestID]);
    if (fail) throw Error('closed tab');
    return {part: {id: 'saved'}};
  };
  let writes = 0;
  const record = async () => { writes++; };
  await assert.rejects(deliverPurchase(pending, send, record), /closed tab/);
  assert.equal(pending.receipt, null);
  fail = false;
  await deliverPurchase(pending, send, record);
  await deliverPurchase(pending, send, record);
  assert.deepEqual(calls, [['web', 8, payload.requestID], ['web', 8, payload.requestID]]);
  assert.equal(writes, 1);
  assert.deepEqual(pending.projects, old.projectLists.web);
  assert.equal(pending.receipt, 'saved');

  await assert.rejects(deliverPurchase({destination: 'http'}, async () => ({}), record), /did not confirm/);
  console.log('PASS single-destination delivery, legacy receipts, and idempotent retry');
})().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
