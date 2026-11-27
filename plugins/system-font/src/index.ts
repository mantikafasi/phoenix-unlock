(() => {
  const ui = phoenix.ui;
  // The native hook also sees inline runs and labels created after navigation.
  // The runtime releases this plugin's font override on unload.
  if (typeof ui.useSystemFont === "function") {
    ui.useSystemFont();
    return;
  }

  if (typeof ui.patchProps !== "function" || typeof ui.onNode !== "function" || typeof ui.onEvent !== "function") return;

  const font = { fontFamily: "sans-serif" };
  const anyNamed = { prefix: "" };
  const fields = ["composer", "user_settings_search_input", "settings-account-title"];
  const parts = ["message.content:", "message.author:", "message.timestamp:"];
  const cleanup: Array<() => void> = [];
  const applied = new Map<number, Pick<Phoenix.NodeInfo, "parent" | "kind"> & { aid: string | null }>();
  const layoutChildren = new Map<number, string>();
  let lastSweep = -Infinity;

  function usesText(node: Phoenix.NodeInfo) {
    if (node.kind != null) {
      return node.kind === "text" || node.kind === "input" || node.kind === "markup";
    }

    // The normal mobile mirror omits kinds. Retain the known chat targets,
    // and try leaf views for labels that have no accessibility ID of their own.
    const aid = node.accessibilityId || "";
    if (aid === "Avatar" || aid.startsWith("message.avatar:") || aid.startsWith("expression.emoji:")) {
      return false;
    }
    return node.children.length === 0
      || fields.includes(aid)
      || parts.some(prefix => aid.startsWith(prefix));
  }

  function walk(roots: number[], mounting = false) {
    const pending = roots.slice();
    const visited = new Set<number>();
    while (pending.length) {
      const id = pending.pop()!;
      if (visited.has(id)) continue;
      const node = ui.node(id);
      if (!node) continue;
      visited.add(id);

      // Each named descendant receives its own mount callback. A screen sweep
      // crosses all of those boundaries, including named and anonymous siblings.
      if (mounting && id !== roots[0] && node.accessibilityId) continue;
      if (usesText(node)) {
        const old = applied.get(id);
        const sameView = old && old.parent === node.parent
          && old.aid === node.accessibilityId && old.kind === node.kind;
        if (mounting || !sameView) {
          ui.update(id, font);
          applied.set(id, { parent: node.parent, aid: node.accessibilityId, kind: node.kind });
        }
      } else {
        applied.delete(id);
      }
      for (const child of node.children) pending.push(child);
    }
    return visited;
  }

  function sweep(extraRoot?: number) {
    lastSweep = Date.now();
    const roots = ui.find(anyNamed);
    const root = ui.root();
    if (root != null) roots.push(root);
    if (extraRoot != null) roots.push(extraRoot);
    const live = walk(roots);
    for (const id of applied.keys()) if (!live.has(id)) applied.delete(id);
    for (const id of layoutChildren.keys()) if (!live.has(id)) layoutChildren.delete(id);
  }

  function mounted(snapshot: Phoenix.NodeInfo) {
    const node = ui.node(snapshot.id);
    if (!node || node.accessibilityId !== snapshot.accessibilityId) return;
    // Even an unchanged ID can denote a newly created Android view.
    walk([node.id], true);
    if (Date.now() - lastSweep >= 1000) sweep(node.id);
  }

  function layoutChanged(event: {node: number}) {
    if (Date.now() - lastSweep >= 1000) {
      sweep(event.node);
      return;
    }
    const node = ui.node(event.node);
    if (!node) return;
    const children = node.children.join(",");
    if (layoutChildren.get(node.id) === children) return;
    layoutChildren.set(node.id, children);
    walk([node.id]);
  }

  // Retain the first version's synchronous patches for newly created chat
  // fields. Other labels are discovered from live nodes, including late cards.
  for (const match of [...fields, ...parts.map(prefix => ({ prefix }))]) {
    cleanup.push(ui.patchProps(match, font));
  }
  cleanup.push(ui.onNode(anyNamed, mounted));
  cleanup.push(ui.onEvent("layout", layoutChanged));
  cleanup.push(ui.onEvent("appeared", ev => {
    // A recycled label may have been rebound with the original font.
    applied.delete(ev.node);
    walk([ev.node]);
  }));
  for (const event of ["pageSettle", "scrollSettle"]) {
    cleanup.push(ui.onEvent(event, ev => sweep(ev.node)));
  }
  phoenix.onUnload(() => {
    for (const remove of cleanup) remove();
    applied.clear();
    layoutChildren.clear();
  });
  sweep();
})();
