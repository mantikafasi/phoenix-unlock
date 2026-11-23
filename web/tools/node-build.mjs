// Build a patched APK from local split files with the same code, libraries, plugins and keys the web page uses.
// usage: node web/tools/node-build.mjs [--package com.x --label Name] [--plugins a,b] [--js my.js,b.js] [--no-runtime] [--frida [--script f.js,g.js]] out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]
//        node web/tools/node-build.mjs --splits [same options but --package/--label] outdir base.apk config.arm64_v8a.apk [config.*.apk ...]
// --plugins picks bundled plugins by name (web/plugins/index.json, default all but example, '' for none); --js adds your own
// (named after the file). --no-runtime leaves the plugin runtime out (only the access patches). --frida embeds
// frida-gadget running --script files (default web/plugins/frida-example.js). --splits patches and signs every
// split on its own, as crates/patcher does.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ABIS, abiOf, buildPatchedApk, buildPatchedSplits } from '../lib/apk.js';
import { INJECTED } from '../lib/host.js';
import { BOOTSTRAP_URL, bundleScripts, GADGET_URLS, gunzip } from '../lib/frida.js';
import { keysFrom } from '../lib/sign.js';
import { readZip } from '../lib/zip.js';

const args = process.argv.slice(2), opt = {};
const flag = (k) => { const i = args.indexOf('--' + k); if (i >= 0) args.splice(i, 1); return i >= 0; };
const splits = flag('splits'), runtime = !flag('no-runtime'), withFrida = flag('frida');
for (const k of ['package', 'label', 'plugins', 'js', 'script']) { const i = args.indexOf('--' + k); if (i >= 0) { opt[k] = args[i + 1]; args.splice(i, 2); } }
const [out, ...apks] = args;
if (apks.length < 2) { console.error('usage: node-build.mjs [--splits] [--package com.x --label Name --plugins a,b --js my.js --no-runtime --frida --script f.js] out base.apk config.<abi>.apk [config.*.apk ...]'); process.exit(1); }

const web = (p) => new URL('../' + p, import.meta.url);
const read = (f) => new Uint8Array(readFileSync(f));
const keys = keysFrom(JSON.parse(readFileSync(web('signing-key.json'), 'utf8')));
const files = apks.map((f) => [basename(f), read(f)]);
const abiIndex = files.findIndex(([, b]) => abiOf(readZip(b)));
if (abiIndex < 0) { console.error(`no ${Object.keys(ABIS).join('/')} split among the inputs`); process.exit(1); }
const abiName = abiOf(readZip(files[abiIndex][1]));
console.log(`ABI: ${abiName}`);
const libraries = Object.entries(ABIS[abiName].libs).map(([name, file]) => ({
  name,
  bytes: read(name === INJECTED && process.env.PHOENIX_LOADER ? process.env.PHOENIX_LOADER : web(file)),
}));
const bundled = JSON.parse(readFileSync(web('plugins/index.json'), 'utf8')).map((p) => p.name);
const picked = opt.plugins === undefined ? bundled.filter((p) => p !== 'example') : opt.plugins.split(',').filter(Boolean);
for (const p of picked) if (!bundled.includes(p)) { console.error(`unknown plugin ${p} (bundled: ${bundled.join(', ')})`); process.exit(1); }
const plugins = !runtime ? null : picked.map((name) => ({ name, source: readFileSync(web(`plugins/${name}.js`), 'utf8') }))
  .concat((opt.js ?? '').split(',').filter(Boolean).map((f) => ({ name: basename(f).replace(/\.[^.]*$/, ''), source: readFileSync(f, 'utf8') })));
const scripts = (opt.script ?? fileURLToPath(web('plugins/frida-example.js'))).split(',').filter(Boolean);
const frida = withFrida ? { gadget: await gunzip(read(web(GADGET_URLS[abiName]))),
  script: bundleScripts(scripts.map((f) => ({ name: basename(f), code: readFileSync(f, 'utf8') })), readFileSync(web(BOOTSTRAP_URL), 'utf8')) } : null;
const log = (s) => console.log(s);
const bytesOf = (parts) => Buffer.concat(parts.map((p) => Buffer.from(p.buffer, p.byteOffset, p.byteLength)));

const t = Date.now();
if (splits) {
  mkdirSync(out, { recursive: true });
  for (const [name, parts] of await buildPatchedSplits({ apks: files, keys, libraries, plugins, frida, log })) writeFileSync(join(out, name), bytesOf(parts));
} else {
  const base = files.find(([, b]) => readZip(b).some((e) => e.name === 'classes.dex'));
  if (!base) { console.error('no base.apk (with classes.dex) among the inputs'); process.exit(1); }
  const configs = files.filter((f, i) => f !== base && i !== abiIndex).map(([, b]) => b);
  const parts = await buildPatchedApk({ base: base[1], abi: files[abiIndex][1], configs, keys, libraries, plugins, frida, packageName: opt.package, label: opt.label, log });
  writeFileSync(out, bytesOf(parts));
}
console.log(`wrote ${out} in ${Date.now() - t} ms`);
