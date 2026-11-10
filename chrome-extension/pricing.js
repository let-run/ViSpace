/* Select a unit price, never the extended total in a table's later columns. */
function priceForQuantity(raw, quantity, singleUnitPrice = '') {
  const tiers = raw.split(';').map(row => {
    const match = row.trim().match(/^(\d[\d,]*)(?:\s*(\+|[-–~]\s*\d[\d,]*))?\s+((?:US\s*\$|USD|EUR|GBP|CNY|[$€£¥])\s*\d[\d,]*(?:\.\d+)?)/i);
    if (!match) return null;
    const minimum = Number(match[1].replace(/,/g, ''));
    const upper = match[2]?.match(/[-–~]\s*([\d,]+)/);
    return {minimum, maximum: upper ? Number(upper[1].replace(/,/g, '')) : Infinity, price: match[3].replace(/\s+/g, ' ') + ' / unit'};
  }).filter(Boolean);
  if (!tiers.length) {
    // A scalar price already applies per unit. Unparsed tier lists must not be saved as a scalar.
    return raw.includes(';') || /^\d[\d,]*\s*(?:\+|[-–~])/.test(raw) ? singleUnitPrice : raw;
  }
  const matching = tiers.filter(tier => quantity >= tier.minimum && quantity <= tier.maximum).sort((a, b) => b.minimum - a.minimum)[0];
  return matching?.price || tiers.find(tier => tier.minimum === 1)?.price || singleUnitPrice;
}
