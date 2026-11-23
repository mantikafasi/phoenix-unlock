// Binary AndroidManifest.xml edits.
// Always: drop requiredSplitTypes / splitTypes / isSplitRequired so the merged APK installs on its own,
//         and set meta-data com.android.vending.splits.required to false (Play Core missing-splits check).
// Optional rename ({packageName, label}): change `package`, provider authorities, com.discord.*
//         permissions and task affinities so it installs next to the original; class names stay as is.
import { buildPool, decodeRaw, encodeRaw, parsePool } from './arsc.js';

const ORIGINAL = 'com.discord';
const REQ_INSTALL_PERM = 'android.permission.REQUEST_INSTALL_PACKAGES';
const DROP_ATTRS = new Set([0x0101064e, 0x0101064f, 0x01010591]); // requiredSplitTypes, splitTypes, isSplitRequired
const A = { name: 0x01010003, value: 0x01010024, label: 0x01010001, authorities: 0x01010018, taskAffinity: 0x01010012 };
const TYPE_STRING = 0x03, TYPE_REFERENCE = 0x01;

export function patchManifest(src, { packageName = ORIGINAL, label = null } = {}) {
  const u8 = new Uint8Array(src);
  const v = new DataView(u8.buffer);
  if (v.getUint16(0, true) !== 0x0003) throw new Error('not a binary XML manifest');
  const renaming = packageName !== ORIGINAL;
  const re = (s) => (s === ORIGINAL ? packageName : s.startsWith(ORIGINAL + '.') ? packageName + s.slice(ORIGINAL.length) : s);

  // pass 1: walk chunks, collect drops and string edits
  const chunks = [];
  let pool = null, poolIdx = -1, resIds = [], appLabelRef = null;
  for (let p = v.getUint16(2, true); p < u8.length; p += v.getUint32(p + 4, true)) chunks.push(p);
  const str = (i) => (i === 0xffffffff ? null : decodeRaw(pool.raws[i], pool.utf8));
  const newStrings = new Map(); // string -> index
  const strIndex = (s) => {
    if (!newStrings.has(s)) newStrings.set(s, pool.raws.length + newStrings.size);
    return newStrings.get(s);
  };
  const edits = new Map(); // attr offset -> string index
  const drops = new Set(); // attr offsets
  const bools = new Set(); // attr offsets set to false
  const stats = { removed: 0, flipped: false, renamed: 0, addedInstallPerm: false };
  let hasInstallPerm = false;
  let samplePermStart = -1, samplePermEnd = -1;

  for (const [ci, p] of chunks.entries()) {
    const type = v.getUint16(p, true), size = v.getUint32(p + 4, true);
    if (type === 0x0001) { pool = parsePool(u8.subarray(p, p + size)); poolIdx = ci; }
    else if (type === 0x0180) resIds = Array.from({ length: (size - 8) / 4 }, (_, i) => v.getUint32(p + 8 + 4 * i, true));
    else if (type === 0x0102) {
      const aStart = p + 16 + v.getUint16(p + 24, true), aSize = v.getUint16(p + 26, true), aCount = v.getUint16(p + 28, true);
      const attrs = Array.from({ length: aCount }, (_, i) => aStart + i * aSize);
      const resOf = (a) => resIds[v.getUint32(a + 4, true)];
      const valStr = (a) => (v.getUint8(a + 15) === TYPE_STRING ? str(v.getUint32(a + 16, true)) : null);
      const tag = str(v.getUint32(p + 20, true));
      if (tag === 'uses-permission') {
        if (attrs.some((a) => resOf(a) === A.name && valStr(a) === REQ_INSTALL_PERM)) hasInstallPerm = true;
        if (samplePermStart < 0 && aCount === 1 && resOf(attrs[0]) === A.name && ci + 1 < chunks.length && v.getUint16(chunks[ci + 1], true) === 0x0103) {
          samplePermStart = ci;
          samplePermEnd = ci + 1;
        }
      }
      for (const a of attrs) if (DROP_ATTRS.has(resOf(a))) drops.add(a);
      if (tag === 'meta-data' && attrs.some((a) => resOf(a) === A.name && valStr(a) === 'com.android.vending.splits.required')) {
        const val = attrs.find((a) => resOf(a) === A.value);
        if (val !== undefined) { bools.add(val); stats.flipped = true; }
      }
      if (!renaming && !label) continue;
      for (const a of attrs) {
        const res = resOf(a), s = valStr(a);
        const set = (ns) => { if (ns !== s) { edits.set(a, strIndex(ns)); stats.renamed++; } };
        if (renaming && tag === 'manifest' && str(v.getUint32(a + 4, true)) === 'package' && s) set(packageName);
        else if (renaming && res === A.authorities && s) set(s.split(';').map(re).join(';'));
        else if (renaming && (tag === 'permission' || tag === 'uses-permission') && res === A.name && s) set(re(s));
        else if (renaming && res === A.taskAffinity && s) set(re(s));
        else if (label && res === A.label) {
          const isRef = v.getUint8(a + 15) === TYPE_REFERENCE, ref = v.getUint32(a + 16, true);
          if (tag === 'application' && isRef) appLabelRef = ref;
          if (isRef && (tag === 'application' || ref === appLabelRef)) { edits.set(a, strIndex(label)); stats.renamed++; }
        }
      }
    }
  }
  if (!pool) throw new Error('manifest has no string pool');

  let installPermIdx = -1;
  if (!hasInstallPerm && samplePermStart >= 0) {
    installPermIdx = strIndex(REQ_INSTALL_PERM);
    stats.addedInstallPerm = true;
  }

  // pass 2: emit
  for (const s of newStrings.keys()) pool.raws.push(encodeRaw(s, pool.utf8));
  const out = [u8.subarray(0, v.getUint16(2, true))];
  for (const [ci, p] of chunks.entries()) {
    const size = v.getUint32(p + 4, true);
    if (ci === poolIdx) { out.push(newStrings.size ? buildPool(pool) : u8.subarray(p, p + size)); continue; }
    if (v.getUint16(p, true) !== 0x0102) {
      out.push(u8.subarray(p, p + size));
      if (ci === samplePermEnd && installPermIdx >= 0) {
        const sp = chunks[samplePermStart], ss = v.getUint32(sp + 4, true);
        const ep = chunks[samplePermEnd], es = v.getUint32(ep + 4, true);
        const startCopy = u8.slice(sp, sp + ss);
        const sv = new DataView(startCopy.buffer);
        const ao = 16 + sv.getUint16(24, true);
        sv.setUint32(ao + 8, installPermIdx, true);
        sv.setUint16(ao + 12, 8, true);
        sv.setUint8(ao + 14, 0);
        sv.setUint8(ao + 15, TYPE_STRING);
        sv.setUint32(ao + 16, installPermIdx, true);
        out.push(startCopy, u8.subarray(ep, ep + es));
      }
      continue;
    }
    const aStart = p + 16 + v.getUint16(p + 24, true), aSize = v.getUint16(p + 26, true), aCount = v.getUint16(p + 28, true);
    const attrs = Array.from({ length: aCount }, (_, i) => aStart + i * aSize);
    if (!attrs.some((a) => drops.has(a) || edits.has(a) || bools.has(a))) { out.push(u8.subarray(p, p + size)); continue; }
    const keep = attrs.filter((a) => !drops.has(a));
    stats.removed += attrs.length - keep.length;
    const head = u8.slice(p, aStart), hv = new DataView(head.buffer);
    const tail = u8.subarray(aStart + aCount * aSize, p + size);
    const n = head.length + keep.length * aSize + tail.length;
    hv.setUint32(4, n, true);
    hv.setUint16(28, keep.length, true);
    for (const off of [30, 32, 34]) { // 1-based id/class/style attribute positions
      const idx = hv.getUint16(off, true);
      if (idx) hv.setUint16(off, keep.indexOf(attrs[idx - 1]) + 1, true);
    }
    const chunk = new Uint8Array(n), cv = new DataView(chunk.buffer);
    chunk.set(head, 0);
    keep.forEach((a, i) => {
      const o = head.length + i * aSize;
      chunk.set(u8.subarray(a, a + aSize), o);
      if (edits.has(a)) {
        const si = edits.get(a);
        cv.setUint32(o + 8, si, true); cv.setUint16(o + 12, 8, true); cv.setUint8(o + 14, 0);
        cv.setUint8(o + 15, TYPE_STRING); cv.setUint32(o + 16, si, true);
      } else if (bools.has(a)) {
        cv.setUint32(o + 8, 0xffffffff, true); cv.setUint8(o + 15, 0x12); cv.setUint32(o + 16, 0, true);
      }
    });
    chunk.set(tail, head.length + keep.length * aSize);
    out.push(chunk);
  }
  const total = out.reduce((s, c) => s + c.length, 0);
  const res = new Uint8Array(total);
  let o = 0;
  for (const c of out) { res.set(c, o); o += c.length; }
  new DataView(res.buffer).setUint32(4, total, true);
  return { data: res, ...stats };
}
