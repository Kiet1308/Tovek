/* Tovek landing — interactions. No dependencies. */
(() => {
  "use strict";
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ---------- Luau highlighter ---------- */
  const KW = new Set("and break continue do else elseif end false for function if in local nil not or repeat return then true until while".split(" "));
  const TYPES = new Set(["number", "string", "boolean", "Vector3", "buffer", "thread", "CFrame", "Instance", "any"]);
  const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  const TOK = /⟦([^|⟧]*)\|([^|⟧]*)(?:\|([^⟧]*))?⟧|⟪|⟫|--[^\n]*|"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`|\d+(?:\.\d+)?(?:e[+-]?\d+)?|[A-Za-z_]\w*|\n|[ \t]+|[^\sA-Za-z_\d]/g;

  function highlight(src) {
    const toks = [];
    let m;
    TOK.lastIndex = 0;
    while ((m = TOK.exec(src))) toks.push(m);
    let html = "";
    let prevNS = "";
    for (let i = 0; i < toks.length; i++) {
      const t = toks[i][0];
      const mk = toks[i];
      if (mk[1] !== undefined) {
        const named = mk[1], anon = mk[2], ev = mk[3] || "";
        const cls = ev ? "nm" : "nm ref";
        const tab = ev ? ' tabindex="0"' : "";
        html += `<b class="${cls}"${tab} data-named="${esc(named)}" data-anon="${esc(anon)}" data-ev="${esc(ev)}">${esc(named)}</b>`;
        prevNS = named;
        continue;
      }
      if (t === "⟪") { html += '<span class="mark">'; continue; }
      if (t === "⟫") { html += "</span>"; continue; }
      if (t === "\n") { html += "\n"; continue; }
      if (/^[ \t]+$/.test(t)) { html += t; continue; }
      let nextNS = "";
      for (let j = i + 1; j < toks.length; j++) {
        const u = toks[j][0];
        if (!/^[ \t]+$/.test(u)) { nextNS = u; break; }
      }
      if (t.startsWith("--")) html += `<span class="c">${esc(t)}</span>`;
      else if (/^["'`]/.test(t)) html += `<span class="s">${esc(t)}</span>`;
      else if (/^\d/.test(t)) html += `<span class="n">${esc(t)}</span>`;
      else if (/^[A-Za-z_]/.test(t)) {
        if (KW.has(t)) html += `<span class="k">${t}</span>`;
        else if (TYPES.has(t) && prevNS === ":") html += `<span class="t">${t}</span>`;
        else if (nextNS === "(" || nextNS === "{" || /^["'`]/.test(nextNS)) html += `<span class="f">${t}</span>`;
        else if (prevNS === "." || prevNS === ":") html += `<span class="p">${t}</span>`;
        else html += esc(t);
      } else html += `<span class="o">${esc(t)}</span>`;
      prevNS = t;
    }
    return html;
  }

  function highlightBC(src) {
    return src.split("\n").map((line) => {
      if (/^\s*REMARK/.test(line)) return `<span class="rem">${esc(line)}</span>`;
      if (/^\s*(--|\[|;)/.test(line)) return `<span class="kk">${esc(line)}</span>`;
      const m = line.match(/^(\s*)([A-Z][A-Z0-9_]*:\s*)?([A-Z][A-Z0-9_]*)(.*)$/);
      if (!m) return esc(line);
      const rest = esc(m[4]).replace(/(\[[^\]]*\])/g, '<span class="kk">$1</span>');
      return `${m[1]}${m[2] ? `<span class="kk">${esc(m[2])}</span>` : ""}<span class="op">${m[3]}</span>${rest}`;
    }).join("\n");
  }

  function wrapLines(html, lineNumbers = true) {
    return html.split("\n").map((l, i) =>
      `<span class="l" data-i="${i + 1}">${lineNumbers ? `<span class="ln">${i + 1}</span>` : ""}${l}</span>`
    ).join("");
  }

  $$("pre[data-lang]").forEach((pre) => {
    const src = pre.textContent.replace(/^\n/, "").replace(/\n\s*$/, "");
    const html = pre.dataset.lang === "bc" ? highlightBC(src) : highlight(src);
    pre.innerHTML = wrapLines(html, pre.dataset.nolines === undefined);
  });

  /* ---------- header tabs pill ---------- */
  const tabs = $(".tabs");
  function placePill() {
    if (!tabs) return;
    const cur = $('a[aria-current="page"]', tabs);
    if (!cur) return;
    const pad = parseFloat(getComputedStyle(tabs).paddingLeft) || 4;
    tabs.style.setProperty("--pill-x", `${cur.offsetLeft - pad}px`);
    tabs.style.setProperty("--pill-w", `${cur.offsetWidth}px`);
  }
  placePill();
  window.addEventListener("resize", placePill);
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(placePill);

  /* ---------- hero reconstruction ---------- */
  const DIS = [
    "[proto 3]  main chunk · 17 instructions",
    "GETIMPORT R0 1 [game]",
    "LOADK R2 K2 ['Players']",
    "NAMECALL R0 R0 K3 ['GetService']",
    "CALL R0 2 1",
    "GETIMPORT R1 1 [game]",
    "LOADK R3 K4 ['ReplicatedStorage']",
    "NAMECALL R1 R1 K3 ['GetService']",
    "CALL R1 2 1",
    "LOADK R4 K5 ['Coin']",
    "NAMECALL R2 R1 K6 ['WaitForChild']",
    "CALL R2 2 1",
    "GETTABLEKS R3 R0 K7 ['PlayerAdded']",
    "DUPCLOSURE R5 K8 []",
    "CAPTURE VAL R2",
    "NAMECALL R3 R3 K9 ['Connect']",
    "CALL R3 2 0",
    "RETURN R0 0",
    "[proto 2]  closure · 1 upvalue",
    "GETTABLEKS R1 R0 K0 ['CharacterAdded']",
    "DUPCLOSURE R3 K1 []",
    "CAPTURE UPVAL U0",
    "NAMECALL R1 R1 K2 ['Connect']",
    "CALL R1 2 0",
    "RETURN R0 0",
    "[proto 1]  closure · 1 upvalue",
    "LOADK R3 K0 ['Humanoid']",
    "NAMECALL R1 R0 K1 ['WaitForChild']",
    "CALL R1 2 1",
    "GETUPVAL R2 0",
    "NAMECALL R2 R2 K2 ['Clone']",
    "CALL R2 1 1",
    "GETIMPORT R3 4 [workspace]",
    "SETTABLEKS R3 R2 K5 ['Parent']",
    "LOADK R5 K6 ['Speed']",
    "NAMECALL R3 R2 K7 ['GetAttribute']",
    "CALL R3 2 1",
    "SETTABLEKS R3 R1 K8 ['WalkSpeed']",
    "MULK R4 R3 K9 [2]",
    "SETTABLEKS R4 R1 K10 ['JumpPower']",
    "GETTABLEKS R4 R1 K11 ['Died']",
    "NEWCLOSURE R6 P0",
    "CAPTURE VAL R2",
    "NAMECALL R4 R4 K12 ['Connect']",
    "CALL R4 2 0",
    "RETURN R0 0",
    "[proto 0]  closure · 1 upvalue",
    "GETUPVAL R0 0",
    "NAMECALL R0 R0 K0 ['Destroy']",
    "CALL R0 1 0",
    "RETURN R0 0",
  ];
  const SRC = [
    'local Players = game:GetService("Players")',
    'local ReplicatedStorage = game:GetService("ReplicatedStorage")',
    'local coin = ReplicatedStorage:WaitForChild("Coin")',
    "Players.PlayerAdded:Connect(function(player)",
    "\tplayer.CharacterAdded:Connect(function(character)",
    '\t\tlocal humanoid = character:WaitForChild("Humanoid")',
    "\t\tlocal clone = coin:Clone()",
    "\t\tclone.Parent = workspace",
    '\t\tlocal speed = clone:GetAttribute("Speed")',
    "\t\thumanoid.WalkSpeed = speed",
    "\t\thumanoid.JumpPower = speed * 2",
    "\t\thumanoid.Died:Connect(function()",
    "\t\t\tclone:Destroy()",
    "\t\tend)",
    "\tend)",
    "end)",
  ];
  const SRC_HTML = SRC.map((l) => highlight(l).replace(/\t/g, "   "));

  function bcHTML(line) {
    if (line.startsWith("[proto")) return `<span class="hd">${esc(line)}</span>`;
    const sp = line.indexOf(" ");
    const op = sp < 0 ? line : line.slice(0, sp);
    const rest = sp < 0 ? "" : line.slice(sp);
    return `<span class="op">${esc(op)}</span>${esc(rest).replace(/(\[[^\]]*\])/g, '<span class="k">$1</span>')}`;
  }

  const recon = $("#recon");
  if (recon) {
    const rowsEl = $(".rows", recon);
    const beam = $(".beam", recon);
    const ROWS = 18;
    const ROW_H = 22;
    const statusIn = $("#ro-in", recon);
    const statusIn2 = $("#ro-in2", recon);
    const statusOut = $("#ro-out", recon);
    const statusOut2 = $("#ro-out2", recon);
    const liveTag = $("#ro-tag", recon);
    const rows = [];
    for (let i = 0; i < ROWS; i++) {
      const r = document.createElement("div");
      r.className = "row bc";
      r.innerHTML = `<span class="ln">${i + 1}</span><span class="tx"></span>`;
      rowsEl.appendChild(r);
      rows.push({ el: r, tx: r.lastElementChild, mode: "", key: "" });
    }

    const setBC = (i, line) => {
      const r = rows[i];
      if (r.mode === "bc" && r.key === line) return;
      r.mode = "bc"; r.key = line;
      r.el.className = "row bc";
      r.tx.innerHTML = bcHTML(line);
    };
    const setSRC = (i) => {
      const r = rows[i];
      if (r.mode === "src") return;
      r.mode = "src"; r.key = "";
      if (i < SRC.length) {
        r.el.className = "row src flash";
        r.tx.innerHTML = SRC_HTML[i];
        r.revealAt = reduced ? 0 : performance.now();
        r.tx.style.clipPath = reduced ? "" : "inset(0 100% 0 0)";
      } else {
        r.el.className = "row bc gone";
      }
    };
    const showFinal = () => {
      for (let i = 0; i < ROWS; i++) setSRC(i);
      statusIn.textContent = "701 B · v9 · 47 instructions";
      statusIn2.textContent = "4 protos · names stripped";
      statusOut.textContent = "16 lines · 8 names inferred";
      statusOut2.textContent = "recompiles ✓ · byte-identical";
      statusOut.classList.remove("dim"); statusOut2.classList.remove("dim");
      liveTag.textContent = "reconstructed"; liveTag.classList.add("live");
    };

    if (reduced) {
      recon.classList.add("reduced");
      showFinal();
      $(".recon-ctl", recon)?.remove();
    } else {
      const T_READ = 1800, T_BUILD = 2700, T_HOLD = 5600, T_RESET = 500;
      const TOTAL = T_READ + T_BUILD + T_HOLD + T_RESET;
      // ?t=<ms> starts the timeline at that offset (used for screenshots / debugging)
      const tOff = +(new URLSearchParams(location.search).get("t") || 0);
      if (tOff) document.documentElement.classList.add("no-intro");
      let start = performance.now() - tOff;
      let paused = false, pausedAt = 0, raf = 0;
      let lastOff = 0;

      const reset = () => {
        for (let i = 0; i < ROWS; i++) { rows[i].mode = ""; rows[i].key = ""; rows[i].revealAt = 0; rows[i].tx.style.clipPath = ""; }
        statusOut.classList.add("dim"); statusOut2.classList.add("dim");
        statusOut.textContent = "—"; statusOut2.textContent = "waiting";
        liveTag.textContent = "reading"; liveTag.classList.remove("live");
        beam.style.opacity = "0";
      };
      reset();

      const frame = (now) => {
        if (paused) return;
        const t = (now - start) % TOTAL;
        for (let i = 0; i < ROWS; i++) {
          const r = rows[i];
          if (r.revealAt) {
            const p = Math.min(1, (now - r.revealAt) / 320);
            const e = 1 - Math.pow(1 - p, 2);
            r.tx.style.clipPath = p >= 1 ? "" : `inset(0 ${((1 - e) * 100).toFixed(1)}% 0 0)`;
            if (p >= 1) r.revealAt = 0;
          }
        }
        if (t < T_READ) {
          // scrolling read of the bytecode listing
          const off = Math.floor(t / 55);
          lastOff = off;
          for (let i = 0; i < ROWS; i++) setBC(i, DIS[(off + i) % DIS.length]);
          const n = Math.min(47, Math.floor((t / T_READ) * 47) + 1);
          statusIn.textContent = `701 B · v9 · ${n} instructions`;
          statusIn2.textContent = "4 protos · names stripped";
          beam.style.opacity = "0";
          liveTag.textContent = "reading";
        } else if (t < T_READ + T_BUILD) {
          const p = (t - T_READ) / T_BUILD;
          const e = p < 0.5 ? 2 * p * p : -1 + (4 - 2 * p) * p; // ease in-out
          const beamRow = e * ROWS;
          beam.style.opacity = "1";
          beam.style.transform = `translateY(${beamRow * ROW_H}px)`;
          for (let i = 0; i < ROWS; i++) {
            if (i < beamRow) setSRC(i);
            else setBC(i, DIS[(lastOff + i) % DIS.length]);
          }
          statusIn.textContent = "701 B · v9 · 47 instructions";
          liveTag.textContent = "reconstructing";
          const lines = Math.min(16, Math.floor(beamRow));
          statusOut.textContent = `${lines} lines`;
          statusOut.classList.remove("dim");
        } else if (t < T_READ + T_BUILD + T_HOLD) {
          beam.style.opacity = "0";
          for (let i = 0; i < ROWS; i++) setSRC(i);
          statusOut.textContent = "16 lines · 8 names inferred";
          statusOut2.textContent = "recompiles ✓ · byte-identical";
          statusOut.classList.remove("dim"); statusOut2.classList.remove("dim");
          liveTag.textContent = "reconstructed"; liveTag.classList.add("live");
        } else {
          // reset: bytecode floods back in from the bottom
          const p = (t - T_READ - T_BUILD - T_HOLD) / T_RESET;
          const upto = ROWS - Math.floor(p * ROWS);
          for (let i = ROWS - 1; i >= upto; i--) setBC(i, DIS[(i) % DIS.length]);
          liveTag.textContent = "reading"; liveTag.classList.remove("live");
          statusOut.classList.add("dim"); statusOut2.classList.add("dim");
        }
        raf = requestAnimationFrame(frame);
      };
      raf = requestAnimationFrame(frame);

      const pauseBtn = $("#recon-pause", recon);
      const replayBtn = $("#recon-replay", recon);
      pauseBtn?.addEventListener("click", () => {
        paused = !paused;
        pauseBtn.textContent = paused ? "▶" : "❚❚";
        pauseBtn.setAttribute("aria-pressed", String(paused));
        if (paused) { pausedAt = performance.now(); cancelAnimationFrame(raf); }
        else { start += performance.now() - pausedAt; raf = requestAnimationFrame(frame); }
      });
      replayBtn?.addEventListener("click", () => {
        start = performance.now();
        reset();
        if (paused) { paused = false; pauseBtn.textContent = "❚❚"; pauseBtn.setAttribute("aria-pressed", "false"); raf = requestAnimationFrame(frame); }
      });
      // save cycles when the panel is off-screen
      const io = new IntersectionObserver((es) => {
        es.forEach((en) => {
          if (en.isIntersecting) { if (!paused) { start += performance.now() - (pausedAt || performance.now()); raf = requestAnimationFrame(frame); } }
          else { pausedAt = performance.now(); cancelAnimationFrame(raf); }
        });
      }, { threshold: 0.05 });
      io.observe(recon);
    }
  }

  /* ---------- naming toggle + evidence tooltips ---------- */
  const nameCode = $("#name-code");
  const nameSwitch = $("#name-switch");
  if (nameCode && nameSwitch) {
    nameSwitch.addEventListener("click", () => {
      const on = nameSwitch.getAttribute("aria-checked") !== "true";
      nameSwitch.setAttribute("aria-checked", String(on));
      $$(".nm", nameCode).forEach((b, i) => {
        b.classList.toggle("anon", !on);
        b.classList.remove("swap");
        void b.offsetWidth;
        b.style.animationDelay = `${Math.min(i * 18, 400)}ms`;
        b.classList.add("swap");
        b.textContent = on ? b.dataset.named : b.dataset.anon;
      });
    });
  }
  const tip = document.createElement("div");
  tip.className = "tip";
  tip.setAttribute("role", "tooltip");
  document.body.appendChild(tip);
  let tipTarget = null;
  function showTip(el) {
    const ev = el.dataset.ev;
    if (!ev) return;
    tipTarget = el;
    tip.innerHTML = `<span class="tl">evidence → <code>${esc(el.dataset.named)}</code></span>${ev.replace(/`([^`]+)`/g, "<code>$1</code>")}`;
    tip.classList.add("show");
    const r = el.getBoundingClientRect();
    tip.style.left = "0px"; tip.style.top = "0px";
    const tw = tip.offsetWidth, th = tip.offsetHeight;
    let x = r.left + r.width / 2 - tw / 2;
    x = Math.max(10, Math.min(window.innerWidth - tw - 10, x));
    let y = r.top - th - 10;
    if (y < 10) y = r.bottom + 10;
    tip.style.left = `${x}px`; tip.style.top = `${y}px`;
  }
  function hideTip() { tip.classList.remove("show"); tipTarget = null; }
  document.addEventListener("mouseover", (e) => { const b = e.target.closest?.(".nm[data-ev]"); if (b && b.dataset.ev) showTip(b); });
  document.addEventListener("mouseout", (e) => { if (e.target.closest?.(".nm") === tipTarget) hideTip(); });
  document.addEventListener("focusin", (e) => { const b = e.target.closest?.(".nm[data-ev]"); if (b && b.dataset.ev) showTip(b); });
  document.addEventListener("focusout", hideTip);
  window.addEventListener("scroll", () => { if (tipTarget) showTip(tipTarget); }, { passive: true });

  /* ---------- compare slider ---------- */
  $$(".cmp").forEach((cmp) => {
    const range = $('input[type="range"]', cmp);
    const set = (v) => cmp.style.setProperty("--x", `${v}%`);
    let touched = false;
    range.addEventListener("input", () => { touched = true; set(range.value); });
    set(range.value);
    if (reduced) return;
    // one slow nudge when the figure first scrolls into view, to invite dragging
    const io = new IntersectionObserver((es) => {
      if (!es.some((e) => e.isIntersecting)) return;
      io.disconnect();
      const from = +range.value, to = Math.min(92, from + 26), t0 = performance.now(), dur = 2200;
      const step = (now) => {
        if (touched) return;
        const p = Math.min(1, (now - t0) / dur);
        const w = 0.5 - 0.5 * Math.cos(p * Math.PI * 2); // out and back
        const v = from + (to - from) * w;
        range.value = v; set(v);
        if (p < 1) requestAnimationFrame(step); else { range.value = from; set(from); }
      };
      setTimeout(() => requestAnimationFrame(step), 500);
    }, { threshold: 0.5 });
    io.observe(cmp);
  });

  /* ---------- tree ↔ code linking ---------- */
  $$(".tree .node[data-lines]").forEach((node) => {
    const target = $(node.dataset.target || "#ui-code");
    if (!target) return;
    const [a, b] = node.dataset.lines.split("-").map(Number);
    const lines = $$(".l", target).filter((l) => { const i = +l.dataset.i; return i >= a && i <= (b || a); });
    const on = () => { node.classList.add("on"); lines.forEach((l) => l.classList.add("hi")); };
    const off = () => { node.classList.remove("on"); lines.forEach((l) => l.classList.remove("hi")); };
    node.addEventListener("mouseenter", on); node.addEventListener("mouseleave", off);
    node.addEventListener("focus", on); node.addEventListener("blur", off);
  });

  /* ---------- reveal + count-up ---------- */
  const fmt = (n, d) => n.toLocaleString("en-US", { minimumFractionDigits: d, maximumFractionDigits: d });
  function countUp(el) {
    const to = parseFloat(el.dataset.to);
    const d = +(el.dataset.dec || 0);
    if (reduced) { el.textContent = fmt(to, d); return; }
    const t0 = performance.now(), dur = 1300;
    const step = (now) => {
      const p = Math.min(1, (now - t0) / dur);
      const e = 1 - Math.pow(1 - p, 3);
      el.textContent = fmt(to * e, d);
      if (p < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }
  const ro = new IntersectionObserver((es) => {
    es.forEach((en) => {
      if (!en.isIntersecting) return;
      en.target.classList.add("in");
      $$("[data-to]", en.target).forEach(countUp);
      if (en.target.matches("[data-to]")) countUp(en.target);
      ro.unobserve(en.target);
    });
  }, { threshold: 0.18, rootMargin: "0px 0px -6% 0px" });
  $$(".reveal, .stat").forEach((el) => ro.observe(el));

  /* ---------- copy buttons ---------- */
  $$(".copy[data-copy]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const src = $(`#${btn.dataset.copy}`);
      if (!src) return;
      try {
        await navigator.clipboard.writeText(src.textContent.trim());
        btn.textContent = "copied"; btn.classList.add("ok");
        setTimeout(() => { btn.textContent = "copy"; btn.classList.remove("ok"); }, 1400);
      } catch { btn.textContent = "select & copy"; }
    });
  });
})();
