const test = (name, run) => { run(); console.log(`PASS: ${name}`); };
const assert = require('assert').strict;
const fs = require('fs');
const vm = require('vm');
const source = fs.readFileSync(require('path').join(__dirname, '../scrape.js'), 'utf8');
function scrape(offers, rows = []) {
  const document = {
    body: {innerText: ''}, querySelector: () => null,
    querySelectorAll(selector) {
      if (selector === 'script[type="application/ld+json"]') return [{textContent: JSON.stringify({'@type': 'Product', mpn: 'TEST', offers})}];
      if (selector === 'tr') return rows.map(innerText => ({innerText}));
      return [];
    }
  };
  return vm.runInNewContext(source + '\nscrapeLCSC()', {document, location: {hostname: 'www.lcsc.com', pathname: '/product-detail/test_C123.html', href: 'https://www.lcsc.com/product-detail/test_C123.html'}, URL});
}
test('captures structured unit price with currency', () => {
  assert.equal(scrape({}).supplier, 'LCSC');
  assert.equal(scrape({}).spn, 'C123');
  assert.equal(scrape({price: '0.1250', priceCurrency: 'USD'}).price, 'USD 0.1250 / unit');
});
test('preserves visible quantity tiers ahead of an aggregate offer', () => {
  assert.equal(scrape({lowPrice: '0.09'}, ['1+\tUS$ 0.15', '100+\tUS$ 0.09']).price, '1+ US$ 0.15; 100+ US$ 0.09');
});
test('does not invent a missing price or use aggregate lowPrice', () => {
  assert.equal(scrape({lowPrice: '0.09'}).price, '');
});
