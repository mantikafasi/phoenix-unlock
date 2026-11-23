// Patch Discord's split APKs like crates/patcher (patchSplit is its `rewrite`), then either sign each split
// (buildPatchedSplits, for `adb install-multiple`) or merge base + ABI split + config splits into one APK
// (buildPatchedApk, what the site offers, since a browser download installs a single APK).
// Beyond the patcher: the plugin runtime can be left out (plugins: null, only the access patches remain), and
// the Frida gadget can ride along (frida.js), loaded through the loader's DT_NEEDED or, without it, the host lib's.
import { mergeArsc } from './arsc.js';
import { patchManifest } from './axml.js';
import { patchDex } from './dex.js';
import { fridaFiles, GADGET } from './frida.js';
import { elfMachine, INJECTED, LIBRARY, patchHost, renameNeeded } from './host.js';
import { signApk } from './sign.js';
import { entryData, readZip, storedEntry, writeZip } from './zip.js';

/** Stored next to the loader, a JSON object of plugin name to source (crates/sigs PLUGIN_BUNDLE). */
export const PLUGIN_BUNDLE = 'libplm.plugins.so';
// Android ABI dir -> its config split and the libraries added next to the host lib (name in the APK -> file
// under web/), the loader (crates/loader) first. The host patch handles x86_64 too; it needs a loader build.
export const ABIS = {
  'arm64-v8a': { split: 'config.arm64_v8a', libs: { [INJECTED]: 'plugins/libphoenix-arm64.so', 'libshadowhook_nothing.so': 'plugins/libshadowhook_nothing-arm64.so' } },
};
/** ABI of a split (the one whose lib/<abi>/ holds the host lib), or undefined. */
export const abiOf = (entries) => Object.keys(ABIS).find((a) => entries.some((e) => e.name === `lib/${a}/${LIBRARY}`));

// JAR signature files and the source stamp, both stale once patched (apksigner drops the stamp too)
const STALE = (name) => /^META-INF\/([^/]+\.(SF|RSA|DSA|EC)|MANIFEST\.MF)$/.test(name) || name === 'stamp-cert-sha256';

/** The plugin bundle as serde_json writes the patcher's BTreeMap: keys sorted, compact, later duplicates win. */
export function pluginBundle(plugins) {
  const map = new Map(plugins.map((p) => [p.name, p.source]));
  return new TextEncoder().encode(`{${[...map].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([k, v]) => `${JSON.stringify(k)}:${JSON.stringify(v)}`).join(',')}}`);
}

/**
 * One split, as crates/patcher's rewrite (plus apksigner): drops stale signature files, forces the launcher in
 * the first dex that holds it, patches every lib/<abi>/liblibdiscord_wasm_host.so and adds the libraries built for its machine
 * plus the plugin bundle next to it. A stored host lib is patched in place in the caller's buffer (saves
 * ~100 MB on phones).
 * @param entries   readZip(split)
 * @param libraries [{name, bytes}] for every ABI; each goes to the host libs of its e_machine
 * @param bundle    pluginBundle(), or null without the plugin runtime (then no libraries are added either)
 * @param frida     {gadget, script} (frida.js) or null
 * @param state     {dex: false, host: []}, shared across the splits of a set
 */
export async function patchSplit(entries, { libraries = [], bundle = null, frida = null, state, log = () => {} }) {
  const needed = bundle ? INJECTED : frida ? GADGET : null;
  const out = [], dirs = [], have = new Set(entries.map((e) => e.name));
  for (const e of entries) {
    if (STALE(e.name)) continue;
    const isDex = !state.dex && /^classes.*\.dex$/.test(e.name);
    const hostDir = e.name.startsWith('lib/') && e.name.endsWith(LIBRARY) ? e.name.slice(0, -LIBRARY.length) : null;
    if (!isDex && hostDir === null) { out.push(e); continue; }
    const data = await entryData(e);
    if (hostDir !== null) {
      const { machine, access } = patchHost(e.name, data, needed);
      log(`${e.name}: access evaluator 0x${access.toString(16)} returns true${needed ? `, liblog.so -> ${needed}` : ''}`);
      dirs.push([hostDir, machine]);
      state.host.push(machine);
      out.push(storedEntry(e.name, data));
    } else {
      const patched = await patchDex(data);
      if (!patched) { out.push(e); continue; }
      log(`${e.name}: launcher returns true`);
      state.dex = true;
      out.push(storedEntry(e.name, patched));
    }
  }
  // stored, so the linker maps them straight out of the APK like the others
  for (const [dir, machine] of dirs) {
    const add = (name, data) => {
      if (have.has(dir + name)) throw new Error(`${dir}${name} is already in the APK`);
      out.push(storedEntry(dir + name, data));
      log(`added ${dir}${name}`);
    };
    if (bundle) {
      const libs = libraries.filter((l) => elfMachine(l.bytes, l.name) === machine);
      if (!libs.some((l) => l.name === INJECTED)) throw new Error(`no ${INJECTED} (loader) built for ${dir}`);
      for (const l of libs) add(l.name, l.bytes);
      add(PLUGIN_BUNDLE, bundle);
    }
    if (!frida) continue;
    if (elfMachine(frida.gadget, GADGET) !== machine) throw new Error(`frida-gadget is not built for ${dir}`);
    for (const [name, data] of fridaFiles(frida.gadget, frida.script, '')) add(name, data);
  }
  return out;
}

/** patchSplit options for a set: plugins null leaves the runtime out; with Frida the loader loads the gadget. */
function options({ libraries, plugins, frida, log }) {
  if (!plugins) return { frida, log };
  if (frida) libraries = libraries.map((l) => (l.name === INJECTED ? { ...l, bytes: renameNeeded(l.name, l.bytes, GADGET) } : l));
  return { libraries, bundle: pluginBundle(plugins), frida, log };
}

const complete = (state) => {
  if (!state.dex || !state.host.length) throw new Error(`incomplete: launcher patched: ${state.dex}, host lib patched: ${state.host.length > 0}. Pass base.apk and the ABI split.`);
};

/**
 * Every split patched and signed on its own, like crates/patcher. Returns [[name, parts]].
 * @param apks [[name, bytes]]; keys keysFrom(signing-key.json); the rest as buildPatchedApk
 */
export async function buildPatchedSplits({ apks, keys, libraries = [], plugins = [], frida = null, log = () => {} }) {
  const state = { dex: false, host: [] }, opts = { ...options({ libraries, plugins, frida, log }), state }, out = [];
  for (const [name, bytes] of apks) out.push([name, writeZip(await patchSplit(readZip(bytes), opts))]);
  complete(state);
  const signed = [];
  for (const [name, zip] of out) { signed.push([name, await signApk(zip, keys)]); log(`signed ${name}`); }
  return signed;
}

/**
 * One installable APK: the patched base and ABI split, with the config splits' resources merged in.
 * @param {Uint8Array} base      base.apk
 * @param {Uint8Array} abi       config.arm64_v8a.apk
 * @param {Uint8Array[]} configs other config splits (density, language)
 * @param keys                   keysFrom(signing-key.json)
 * @param libraries              [{name, bytes}] added next to the host lib (ABIS[abi].libs) with the plugin runtime
 * @param plugins                [{name, source}] for the plugin bundle, or null to leave the plugin runtime out
 * @param frida                  {gadget, script} to embed frida-gadget (frida.js), or null
 * @param packageName            install under another package (next to the original); default com.discord, as the patcher
 * @param label                  launcher name, e.g. when renamed
 * @returns {Promise<Uint8Array[]>} parts of the signed APK (new Blob(parts))
 */
export async function buildPatchedApk({ base, abi, configs = [], keys, libraries = [], plugins = [], frida = null, packageName = 'com.discord', label = null, log = () => {} }) {
  if (!/^[a-zA-Z][\w]*(\.[a-zA-Z][\w]*)+$/.test(packageName)) throw new Error(`invalid package name: ${packageName}`);
  const state = { dex: false, host: [] }, opts = { ...options({ libraries, plugins, frida, log }), state };
  log(plugins ? `plugins: ${plugins.map((p) => p.name).join(', ') || 'none'}` : 'plugin runtime: off');
  const baseEntries = await patchSplit(readZip(base), opts);
  const abiEntries = await patchSplit(readZip(abi), opts);
  complete(state);
  const cfgEntries = configs.map(readZip);
  const have = new Set(baseEntries.map((e) => e.name));
  const out = [];

  for (const e of baseEntries) {
    if (e.name === 'AndroidManifest.xml') {
      const m = patchManifest(await entryData(e), { packageName, label });
      log(`manifest: removed ${m.removed} split attribute(s), splits.required=false: ${m.flipped}` +
        (m.addedInstallPerm ? ', +REQUEST_INSTALL_PACKAGES' : '') +
        (m.renamed ? `, ${m.renamed} value(s) renamed to ${packageName}${label ? ` / "${label}"` : ''}` : ''));
      out.push(storedEntry(e.name, m.data));
    } else if (e.name === 'resources.arsc') {
      const splits = [];
      for (const z of cfgEntries) { const a = z.find((x) => x.name === 'resources.arsc'); if (a) splits.push(await entryData(a)); }
      out.push(storedEntry(e.name, mergeArsc(await entryData(e), splits, packageName === 'com.discord' ? null : packageName)));
      log(`resources: merged ${splits.length} config split table(s)`);
    } else out.push(e);
  }
  for (const e of abiEntries) if (e.name.startsWith('lib/') && !have.has(e.name)) out.push(e);

  let added = 0;
  for (const z of cfgEntries)
    for (const e of z)
      if (e.name.startsWith('res/') && !have.has(e.name)) { out.push(e); have.add(e.name); added++; }
  log(`resources: added ${added} file(s) from config splits`);

  const zip = writeZip(out);
  log('signing');
  return signApk(zip, keys, log);
}
