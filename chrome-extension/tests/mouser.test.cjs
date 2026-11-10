const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const {join} = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

test('reads a regional Mouser detail page and uses unit prices instead of totals', () => {
  const cells = values => values.map(textContent => ({textContent}));
  const rows = [
    {querySelectorAll: () => cells(['1:', '$2.75', '$2.75'])},
    {querySelectorAll: () => cells(['100:', '$1.40', '$140.00'])},
  ];
  const table = {querySelectorAll: selector => selector === 'th'
    ? cells(['Qty.', 'Unit Price', 'Ext. Price']) : rows};
  const script = {textContent: JSON.stringify({'@graph': [{
    '@type': 'Product', image: '/images/part.jpg',
  }]})};
  const context = vm.createContext({
    URL,
    location: new URL('https://eu.mouser.com/en/ProductDetail/Example/ABC'),
    document: {
      body: {innerText: 'Mouser No:\n123-ABC\nMfr. No: ABC\nMfr.: Example\nDescription: Test resistor\nPackage / Case: 0402\nResistance: 10 kΩ\nTolerance: 1%'},
      querySelector: () => null,
      querySelectorAll: selector => ({
        'script[type="application/ld+json"]': [script],
        'a[href]': [{textContent: 'ABC Datasheet', href: 'https://eu.mouser.com/datasheet/ABC.pdf'}],
        table: [table],
      }[selector] || []),
    },
  });
  vm.runInContext(readFileSync(join(__dirname, '../mouser.js'), 'utf8'), context);
  vm.runInContext(readFileSync(join(__dirname, '../pricing.js'), 'utf8'), context);
  const part = vm.runInContext('scrapeMouser()', context);
  assert.equal(part.supplier, 'Mouser');
  assert.equal(part.spn, '123-ABC');
  assert.equal(part.number, 'ABC');
  assert.equal(part.manufacturer, 'Example');
  assert.equal(part.package, '0402');
  assert.equal(part.value, '10 kΩ');
  assert.equal(part.tolerance, '1%');
  assert.equal(part.image, 'https://eu.mouser.com/images/part.jpg');
  assert.equal(part.datasheet, 'https://eu.mouser.com/datasheet/ABC.pdf');
  context.part = part;
  assert.equal(vm.runInContext('priceForQuantity(part.price, 100, part.singleUnitPrice)', context), '$1.40 / unit');
});
