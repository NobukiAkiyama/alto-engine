const $ = (s) => document.querySelector(s);
export function installM5({
  engine,
  getDocument,
  getSelected,
  select,
  attach,
  run,
  fail,
}) {
  let shape = null,
    gesture = null,
    outlineKey = "",
    outlineTicket = 0,
    lastSelected;
  const existing = () => {
    const d = getDocument();
    const id = getSelected();
    return { d, id, n: id && d ? d.view.node(id) : null };
  };
  function draw() {
    const svg = $("#selection-overlay"),
      r = engine.canvas.getBoundingClientRect(),
      ratio = Math.min(devicePixelRatio || 1, 2),
      scale = engine.viewport.zoom / ratio,
      a = engine.viewport.rotation,
      c = Math.cos(a),
      s = Math.sin(a),
      [cx, cy] = engine.viewport.center;
    svg.setAttribute("viewBox", `0 0 ${r.width} ${r.height}`);
    $("#selection-transform").setAttribute(
      "transform",
      `matrix(${c * scale} ${s * scale} ${-s * scale} ${c * scale} ${r.width / 2 - scale * (c * cx - s * cy)} ${r.height / 2 - scale * (s * cx + c * cy)})`,
    );
  }
  function preview() {
    if (!gesture) {
      $("#selection-preview").setAttribute("d", "");
      return;
    }
    const p = gesture.points,
      a = p[0],
      b = p.at(-1),
      x = Math.min(a[0], b[0]),
      y = Math.min(a[1], b[1]),
      w = Math.abs(a[0] - b[0]),
      h = Math.abs(a[1] - b[1]);
    let path;
    if (gesture.kind === "rectangle") path = `M${x},${y}h${w}v${h}h${-w}Z`;
    else if (gesture.kind === "ellipse")
      path = `M${x},${y + h / 2}a${w / 2},${h / 2} 0 1 0 ${w},0a${w / 2},${h / 2} 0 1 0 ${-w},0`;
    else
      path = p.map((v, i) => `${i ? "L" : "M"}${v[0]},${v[1]}`).join("") + "Z";
    $("#selection-preview").setAttribute("d", path);
    draw();
  }
  function bind() {
    const canvas = engine.canvas;
    canvas.addEventListener(
      "pointerdown",
      (e) => {
        if (!shape || e.button !== 0 || e.altKey) return;
        e.stopImmediatePropagation();
        canvas.setPointerCapture(e.pointerId);
        gesture = {
          kind: shape,
          points: [engine.viewport.screenToDocument(e.clientX, e.clientY)],
        };
        preview();
      },
      true,
    );
    canvas.addEventListener(
      "pointermove",
      (e) => {
        if (!gesture) return;
        e.stopImmediatePropagation();
        const p = engine.viewport.screenToDocument(e.clientX, e.clientY);
        if (gesture.kind === "lasso") {
          if (gesture.points.length < 8192) gesture.points.push(p);
        } else gesture.points[1] = p;
        preview();
      },
      true,
    );
    canvas.addEventListener(
      "pointerup",
      (e) => {
        if (!gesture) return;
        e.stopImmediatePropagation();
        const g = gesture;
        gesture = null;
        preview();
        const d = getDocument();
        if (!d) return;
        const a = g.points[0],
          b = g.points.at(-1);
        run(async () => {
          await d.selection.apply({
            kind: g.kind,
            mode: $("#selection-mode").value,
            feather: Number($("#selection-feather").value),
            x: a[0],
            y: a[1],
            width: b[0] - a[0],
            height: b[1] - a[1],
            points: g.points,
          });
          refresh();
        });
      },
      true,
    );
    canvas.addEventListener(
      "pointercancel",
      () => {
        gesture = null;
        preview();
      },
      true,
    );
  }
  function refresh() {
    const { d, id, n } = existing();
    for (const element of document.querySelectorAll(
      "[data-selection],[data-tool],#select-all,#select-clear,#select-invert,#add-layer,#add-group,#save,#import",
    ))
      element.disabled = !d;
    for (const control of [
      "mask-selection",
      "mask-white",
      "clip-to-below",
      "add-blur",
      "add-adjustment",
    ])
      $("#" + control).disabled = !n;
    if (!d) return;
    $("#selection-state").textContent = d.view.selection
      ? Object.keys(d.view.selection.tiles).length
        ? "選択あり"
        : "空の選択範囲"
      : "選択なし";
    const key = d.view.root.id + JSON.stringify(d.view.selection);
    if (key !== outlineKey) {
      outlineKey = key;
      const ticket = ++outlineTicket;
      d.selection
        .outline()
        .then((path) => {
          if (ticket !== outlineTicket) return;
          $("#outline-black").setAttribute("d", path);
          $("#outline-white").setAttribute("d", path);
          draw();
        })
        .catch(fail);
    }
    if (lastSelected !== id) {
      $("#mask-edit").checked = false;
      lastSelected = id;
    }
    if (!n) return;
    $("#clip-to-below").checked = n.props.clip_to_below;
    $("#mask-enabled").checked = n.props.mask_enabled;
    $("#mask-linked").checked = n.props.mask_linked;
    for (const control of [
      "mask-enabled",
      "mask-linked",
      "mask-edit",
      "mask-remove",
    ])
      $("#" + control).disabled = !n.props.mask;
    if (!n.props.mask) $("#mask-edit").checked = false;
    $("#effect-list").replaceChildren();
    n.props.effects.forEach((e, i) => {
      const row = document.createElement("div");
      row.className = "effect-row";
      const label = document.createElement("span");
      label.textContent =
        (e.type === "gaussian_blur" ? `ぼかし ${e.radius} px` : "色調整") +
        (e.selection ? "・選択付き" : "");
      row.append(label);
      if (e.type === "gaussian_blur") {
        const radius = document.createElement("input");
        radius.type = "number";
        radius.min = "0";
        radius.max = "64";
        radius.value = e.radius;
        radius.style.width = "62px";
        radius.setAttribute("aria-label", `ぼかし半径 効果${i + 1}`);
        radius.onchange = () =>
          run(() =>
            d.effects.set(
              id,
              n.props.effects.map((effect, index) =>
                index === i
                  ? { ...effect, radius: Number(radius.value) }
                  : effect,
              ),
              { useSelection: false },
            ),
          );
        row.append(radius);
      }

      for (const [text, action] of [
        [
          "↑",
          () => {
            if (i === 0) return;
            const a = [...n.props.effects];
            [a[i - 1], a[i]] = [a[i], a[i - 1]];
            return d.effects.set(id, a, { useSelection: false });
          },
        ],
        [
          "↓",
          () => {
            if (i === n.props.effects.length - 1) return;
            const a = [...n.props.effects];
            [a[i], a[i + 1]] = [a[i + 1], a[i]];
            return d.effects.set(id, a, { useSelection: false });
          },
        ],
        [
          "外す",
          () =>
            d.effects.set(
              id,
              n.props.effects.filter((_, j) => j !== i),
              { useSelection: false },
            ),
        ],
      ]) {
        const b = document.createElement("button");
        b.textContent = text;
        b.onclick = () => run(action);
        row.append(b);
      }
      $("#effect-list").append(row);
    });
    const adjustment = n.kind.kind === "adjustment" ? n.kind.adjustment : null;
    $("#update-adjustment").disabled = !adjustment;
    if (adjustment)
      for (const k of ["brightness", "contrast", "saturation"])
        $("#adjust-" + k).value = adjustment[k] ?? (k === "brightness" ? 0 : 1);
    draw();
  }
  for (const b of document.querySelectorAll("[data-panel]"))
    b.onclick = () =>
      document
        .getElementById(b.dataset.panel)
        .scrollIntoView({ block: "start", behavior: "smooth" });
  for (const b of document.querySelectorAll("[data-selection]"))
    b.onclick = () => {
      shape = b.dataset.selection;
      for (const other of document.querySelectorAll(
        "[data-selection],[data-tool]",
      ))
        other.classList.toggle("active-tool", other === b);
    };
  for (const b of document.querySelectorAll("[data-tool]"))
    b.addEventListener("click", () => {
      shape = null;
      for (const other of document.querySelectorAll(
        "[data-selection],[data-tool]",
      ))
        other.classList.toggle("active-tool", other === b);
    });
  for (const [id, action] of [
    ["select-all", "all"],
    ["select-clear", "clear"],
    ["select-invert", "invert"],
  ])
    $("#" + id).onclick = () => run(() => getDocument()?.selection[action]());
  for (const [id, prop] of [
    ["clip-to-below", "clip_to_below"],
    ["mask-enabled", "mask_enabled"],
    ["mask-linked", "mask_linked"],
  ])
    $("#" + id).onchange = () =>
      run(() => {
        const { d, id: node } = existing();
        return d.transact("マスク設定", (tx) =>
          tx.setProps(node, { [prop]: $("#" + id).checked }),
        );
      });
  for (const [id, action] of [
    ["mask-selection", "fromSelection"],
    ["mask-white", "addWhite"],
    ["mask-remove", "remove"],
  ])
    $("#" + id).onclick = () =>
      run(() => {
        const { d, id: node } = existing();
        return d.masks[action](node);
      });
  $("#add-blur").onclick = () =>
    run(() => {
      const { d, id, n } = existing();
      const old = n.props.effects.map((e) => ({
        ...e,
        selection: e.selection ?? null,
      }));
      return d.effects.set(id, [
        ...old,
        { type: "gaussian_blur", radius: Number($("#blur-radius").value) },
      ]);
    });
  const adjustment = () => ({
    type: "color_adjust",
    brightness: Number($("#adjust-brightness").value),
    contrast: Number($("#adjust-contrast").value),
    saturation: Number($("#adjust-saturation").value),
  });
  $("#add-adjustment").onclick = () =>
    run(async () => {
      const { d, n } = existing();
      let id;
      await d.transact("色調整レイヤー追加", (tx) => {
        id = tx.addAdjustment(adjustment(), {
          parent: n?.kind.kind === "group" ? n.id : n?.parent || d.view.root.id,
        });
      });
      select(id);
    });
  $("#update-adjustment").onclick = () =>
    run(() => {
      const { d, id } = existing();
      return d.transact("色調整を変更", (tx) =>
        tx.setAdjustment(id, adjustment()),
      );
    });
  $("#sample-m5").onclick = () =>
    run(async () => {
      const blob = await (
        await fetch("../../verification/sample-m5.alto")
      ).blob();
      attach(await engine.openDocument(blob));
    });
  $("#test-m5").onclick = () =>
    run(async () => {
      const b = $("#test-m5");
      b.disabled = true;
      $("#error").textContent = "";
      $("#test-results").textContent = "M5を検証中…";
      try {
        const { runM5Tests } = await import("../../tests/e2e/m5-tests.js");
        await runM5Tests(
          engine,
          (line) => ($("#test-results").textContent += "\n" + line),
        );
        attach(engine.document);
      } finally {
        b.disabled = false;
      }
    });
  window.addEventListener("keydown", (e) => {
    if (
      ["INPUT", "TEXTAREA", "SELECT"].includes(
        document.activeElement.tagName,
      ) ||
      !(e.metaKey || e.ctrlKey)
    )
      return;
    if (e.key.toLowerCase() === "d") {
      e.preventDefault();
      run(() => getDocument()?.selection.clear());
    }
    if (e.key.toLowerCase() === "a") {
      e.preventDefault();
      run(() => getDocument()?.selection.all());
    }
  });
  engine.on("viewport", draw);
  engine.on("frame", draw);
  engine.on("change", () => queueMicrotask(refresh));
  engine.on("canvasReplaced", () => {
    bind();
    outlineKey = "";
    refresh();
  });
  bind();
  return { refresh };
}
