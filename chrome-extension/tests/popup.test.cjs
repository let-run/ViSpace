const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const {join} = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const {restorePendingPurchase} = require('../transport.js');

test('a saved purchase does not prevent switching destinations and loading projects', async () => {
  const elements = new Map();
  function element() {
    return {
      value: '',
      children: [],
      dataset: {},
      get options() { return this.children; },
      append(...children) { this.children.push(...children); },
      replaceChildren() { this.children = []; },
      querySelectorAll() { return []; },
      addEventListener() {},
      setAttribute() {},
    };
  }
  const get = id => {
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  };
  const requests = [];
  const context = vm.createContext({
    URL,
    document: {getElementById: get, createElement: element, querySelectorAll: () => []},
    restorePendingPurchase,
    inventoryTabRequest: async (tab, op) => {
      requests.push([tab, op]);
      return op === 'health' ? {application: 'bom-inventory', protocol: 1} : [{name: 'Example'}];
    },
    chrome: {
      storage: {local: {
        get: async defaults => ({...defaults, address: 'wss://127.0.0.1', port: '8081', pendingPurchase: {
          destination: 'http', receipt: 'saved',
          payload: {part: {number: 'Old part'}, quantity: 1, assignments: []},
        }}),
        set: async () => {},
      }},
      tabs: {query: async () => [{id: 42, title: 'ViSpace', url: 'https://localhost:8787/'}]},
      permissions: {request: async () => true},
    },
  });
  await vm.runInContext(readFileSync(join(__dirname, '../popup.js'), 'utf8'), context);

  assert.equal(get('address').value, 'wss://127.0.0.1:8081');
  assert.equal(vm.runInContext('endpoint()', context), 'https://127.0.0.1:8081');
  get('address').value = 'wss://[::1]:8824';
  assert.equal(vm.runInContext('endpoint()', context), 'https://[::1]:8824');
  assert.equal(get('tab-web').disabled, false);
  assert.equal(get('connect').disabled, false);
  assert.deepEqual(requests, []);
  get('tab-web').onclick();
  get('web-tab').value = '42';
  await vm.runInContext('connect()', context);

  assert.deepEqual(requests, [[42, 'health'], [42, 'projects']]);
  assert.equal(get('projects').children[0].children[1].textContent, 'Example');
  assert.equal(get('submit').disabled, true);
});
