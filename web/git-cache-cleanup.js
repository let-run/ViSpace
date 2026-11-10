// Raw cache pruning requires a full current baseline and proven repository ancestry.
export function redundantGitRanges(ranges, head, reachable, preserve) {
  if(preserve||!ranges.some(r=>r.bases.length===0&&r.tips.includes(head)))return [];
  const known=new Set(reachable);
  return ranges.filter(r=>r.tips.length>0&&!r.tips.includes(head)&&r.tips.every(t=>known.has(t)));
}
