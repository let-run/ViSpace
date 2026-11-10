const assert = require('assert').strict;
const fs = require('fs');
const vm = require('vm');
const cell = textContent => ({textContent});
const row = values => ({querySelectorAll: () => values.map(cell)});
const attributes = [row(['DigiKey Part Number', '296-TEST-2-ND - Tape & Reel (TR) 296-TEST-1-ND - Cut Tape (CT)']), row(['Manufacturer Product Number', 'TEST123']), row(['Manufacturer', 'Example']), row(['Description', 'Test regulator']), row(['Package / Case', 'QFN'])];
const table = values => ({querySelectorAll: () => values.map(row)});
const document = {
  body: {innerText: ''}, querySelector: () => null,
  querySelectorAll(selector) {
    if (selector === 'tr') return attributes;
    if (selector === 'table') return [table([['3000', '$0.10', '$300']]), table([['1', '$0.50', '$0.50'], ['10', '$0.40', '$4']])];
    if (selector === 'a[href]') return [{textContent: 'Datasheet', href: 'https://example.com/test.pdf'}];
    if (selector === 'script[type="application/ld+json"]') return [{textContent: JSON.stringify({'@type': 'Product', image: 'https://example.com/test.jpg'})}];
    return [];
  }
};
const source = fs.readFileSync(require('path').join(__dirname, '../digikey.js'), 'utf8');
const result = vm.runInNewContext(source + '\nscrapeDigiKey()', {document, location: {hostname: 'www.digikey.com', pathname: '/en/products/detail/example/TEST123/1234', href: 'https://www.digikey.com/en/products/detail/example/TEST123/1234'}, URL});
assert.equal(result.supplier, 'DigiKey');
assert.equal(result.spn, '296-TEST-1-ND');
assert.equal(result.number, 'TEST123');
assert.equal(result.price, '1+ $0.50; 10+ $0.40');
assert.equal(result.datasheet, 'https://example.com/test.pdf');
assert.equal(result.image, 'https://example.com/test.jpg');
assert.equal(result.lcsc, undefined);
console.log('PASS: DigiKey supplier, SKU, attributes, attachments, and packaging-specific pricing');
