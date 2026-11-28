(() => {
  "use strict";

  const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  const reduced = () => motionQuery.matches;
  const $ = (sel) => document.querySelector(sel);

  /* ---------- Copy buttons ---------- */
  /* In a sandboxed or insecure context the Clipboard API is refused, so the
     command is selected instead and the button says which keys finish the copy. */
  const status = $("#copy-status");
  const copyKeys = /Mac|iPhone|iPad/.test(navigator.userAgent) ? "⌘C" : "Ctrl+C";
  document.querySelectorAll("button.copy[data-copy]").forEach((btn) => {
    const label = btn.textContent;
    let timer = 0;
    btn.addEventListener("click", async () => {
      const src = document.getElementById(btn.dataset.copy);
      if (!src) return;
      let msg;
      try {
        await navigator.clipboard.writeText(src.textContent.trim());
        btn.textContent = "Copied";
        msg = "Copied to the clipboard.";
      } catch (e) {
        const range = document.createRange();
        range.selectNodeContents(src);
        const sel = window.getSelection();
        sel.removeAllRanges();
        sel.addRange(range);
        btn.textContent = "Press " + copyKeys;
        msg = "Selected. Press " + copyKeys + " to copy.";
      }
      if (status) status.textContent = msg;
      clearTimeout(timer);
      timer = setTimeout(() => { btn.textContent = label; }, 2400);
    });
  });

  /* ---------- Deep links to one install method ---------- */
  /* "#install-linux" and "#install-source" open that method's panel (the
     panels share one name, so the others close) and move focus to it. */
  const openInstall = (hash, focus) => {
    /* getElementById, not querySelector: a hash is not a valid selector in general
       ("#install-a/b"), and a SyntaxError here would stop the rest of this script. */
    const d = hash && hash.startsWith("#install-") ? document.getElementById(hash.slice(1)) : null;
    if (!d || d.tagName !== "DETAILS") return false;
    d.open = true;
    if (focus) {
      const summary = d.querySelector("summary");
      summary.scrollIntoView({ block: "start" });
      summary.focus({ preventScroll: true });
    }
    return true;
  };
  document.querySelectorAll('a[href^="#install-"]').forEach((a) => {
    /* Following the hash would move focus back to the page, so scroll and focus here instead. */
    a.addEventListener("click", (e) => { if (openInstall(a.getAttribute("href"), true)) e.preventDefault(); });
  });
  openInstall(location.hash, false);

  /* ---------- Orchestrator + board demo ---------- */
  const WORKERS = {
    1: {
      badge: "! approval", run: false,
      title: "1 · vt-parser · claude · orch/vt-parser",
      screen:
        '<span class="d"> Edit file</span>  crates/noa-vt/src/parser.rs\n' +
        '<span class="d"> ─────────────────────────────────────</span>\n' +
        '<span class="r">  - Some(0x9c) =&gt; self.osc_end(),</span>\n' +
        '<span class="g">  + Some(0x9c) if self.c1 =&gt; self.osc_end(),</span>\n\n' +
        ' Do you want to make this edit to parser.rs?\n' +
        '<span class="a"> ❯ 1. Yes</span>\n' +
        '   2. Yes, and don\'t ask again this session\n' +
        '   3. No, and tell Claude what to do differently',
    },
    2: {
      badge: "? question", run: false,
      title: "2 · cjk-reflow · codex · orch/cjk-reflow",
      screen:
        '<span class="d">• </span>The reflow test passes once wide cells stay\n' +
        '  inside the DECLRMM margins. Two options:\n\n' +
        '  1. clip a wide cell at the right margin\n' +
        '  2. wrap it to the next row inside the margins\n\n' +
        '  Which one should I keep?\n\n' +
        '<span class="a">› </span><span class="d">waiting for your answer</span>',
    },
    3: {
      badge: "● working", run: true,
      title: "3 · linux-ci · agy · orch/linux-ci · 1 worker",
      screen:
        '<span class="a">$</span> scripts/package-linux.sh\n' +
        '<span class="g">   Compiling</span> noa-render v0.9.4\n' +
        '<span class="g">   Compiling</span> noa-app v0.9.4\n' +
        '<span class="g">   Compiling</span> noa v0.9.4\n' +
        '<span class="d">  spawned icons-meta to check the desktop entry</span>\n' +
        '<span class="d">  and AppStream metadata while this builds</span>',
    },
    4: {
      badge: "● working", run: true,
      title: "4 · icons-meta · agy · under linux-ci",
      screen:
        '<span class="d">• </span>Reading dist/linux/\n' +
        '<span class="d">• </span>Checking the desktop entry\n' +
        '<span class="d">• </span>Checking the AppStream metadata\n\n' +
        '<span class="d">  started by linux-ci, stowed on the same board;</span>\n' +
        '<span class="d">  closing linux-ci closes it too</span>',
    },
  };
  const QUEUE = ["1", "2"];

  const board = $("#board");
  const rows = Array.from(board.querySelectorAll('[role="option"]'));
  const pvBadge = $("#pv-badge");
  const pvTitle = $("#pv-title");
  const pvScreen = $("#pv-screen");
  let selected = "1";

  function select(id) {
    const w = WORKERS[id];
    if (!w) return;
    selected = id;
    rows.forEach((row) => row.setAttribute("aria-selected", String(row.dataset.id === id)));
    board.setAttribute("aria-activedescendant", "w-" + id);
    pvBadge.textContent = w.badge;
    pvBadge.classList.toggle("badge--run", w.run);
    pvTitle.textContent = w.title;
    pvScreen.innerHTML = w.screen;
    const row = document.getElementById("w-" + id);
    if (row && row.scrollIntoView && board.scrollHeight > board.clientHeight) row.scrollIntoView({ block: "nearest" });
  }

  rows.forEach((row) => {
    row.addEventListener("click", () => { select(row.dataset.id); board.focus({ preventScroll: true }); });
  });

  board.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const ids = rows.map((r) => r.dataset.id);
    const i = ids.indexOf(selected);
    let next = null;
    if (e.key === "ArrowDown" || e.key === "j") next = ids[Math.min(i + 1, ids.length - 1)];
    else if (e.key === "ArrowUp" || e.key === "k") next = ids[Math.max(i - 1, 0)];
    else if (e.key === "Home") next = ids[0];
    else if (e.key === "End") next = ids[ids.length - 1];
    else if (/^[1-4]$/.test(e.key)) next = e.key;
    else if (e.key === "n") {
      const q = QUEUE.indexOf(selected);
      next = QUEUE[(q + 1) % QUEUE.length];
    }
    if (next) { e.preventDefault(); select(next); }
  });

  select("1");

  /* Reveal the orchestrator's lines and tick the wait timers while visible. */
  const orchLines = Array.from(document.querySelectorAll("#orch .ln"));
  const waits = Array.from(document.querySelectorAll(".row__tm[data-wait]"));
  let demoVisible = false;
  let revealed = false;
  let elapsed = 0;

  function reveal() {
    if (revealed) return;
    revealed = true;
    if (reduced()) { orchLines.forEach((l) => l.classList.add("on")); return; }
    orchLines.forEach((l, n) => setTimeout(() => l.classList.add("on"), 120 + n * 260));
  }

  setInterval(() => {
    if (!demoVisible || reduced() || document.hidden) return;
    elapsed += 1;
    waits.forEach((el) => { el.textContent = Number(el.dataset.wait) + elapsed + "s"; });
  }, 1000);

  /* ---------- Hero log stream (illustration) ---------- */
  const flood = $("#flood-lines");
  const floodScreen = flood && flood.parentElement;
  let floodVisible = false;
  let floodRaf = 0;
  let floodRun = () => {};

  if (flood) {
    const crates = ["noa-core", "noa-vt", "noa-grid", "noa-font", "noa-theme", "noa-config", "noa-pty", "noa-ipc", "noa-render", "noa-app"];
    const tests = ["screen::reflow::wraps_cjk", "deferred::wide_tail", "osc::hyperlink_8", "kitty::push_pop_flags", "grapheme::zwj_family", "sgr::curly_underline"];
    const wide = ["進捗 100%", "한글 입력 통과", "宽字符渲染 完成", "絵文字 👩‍💻 🇯🇵", "ログを流し込み中"];
    const esc = (t) => t.replace(/&/g, "&amp;").replace(/</g, "&lt;");
    const lines = [];
    for (let i = 0; i < 48; i++) {
      const k = i % 6;
      if (k === 0 || k === 3) lines.push('<span class="g">   Compiling</span> ' + crates[i % crates.length] + " v0.9.23");
      else if (k === 1) lines.push("test " + tests[i % tests.length] + " ... " + '<span class="g">ok</span>');
      else if (k === 2) lines.push('<span class="d">[' + String(i * 37 % 1000).padStart(3, "0") + "]</span> " + esc(wide[i % wide.length]));
      else if (k === 4) lines.push('<span class="d">  warning:</span> unused import in ' + crates[(i + 3) % crates.length]);
      else lines.push('<span class="a">  →</span> ' + "0x" + (i * 2654435761 >>> 0).toString(16).padStart(8, "0") + " streamed");
    }
    const block = lines.join("\n");
    flood.innerHTML = block + "\n" + block;

    let y = 0;
    let last = 0;
    const step = (t) => {
      floodRaf = 0;
      if (!floodVisible || reduced() || document.hidden) { last = 0; return; }
      if (last) y += (t - last) * 0.09;
      last = t;
      const half = (flood.scrollHeight - 24) / 2;
      if (half > 0 && y >= half) y -= half;
      flood.style.transform = "translateY(" + (-y).toFixed(1) + "px)";
      floodRaf = requestAnimationFrame(step);
    };
    const run = floodRun = () => { if (!floodRaf && floodVisible && !reduced()) floodRaf = requestAnimationFrame(step); };
    document.addEventListener("visibilitychange", run);
    motionQuery.addEventListener("change", () => { if (reduced()) flood.style.transform = ""; else run(); });
  }

  /* ---------- Start things when they scroll into view ---------- */
  const demo = $(".demo");
  if ("IntersectionObserver" in window) {
    const io = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (entry.target === demo) {
          demoVisible = entry.isIntersecting;
          if (entry.isIntersecting) reveal();
        } else if (entry.target === floodScreen) {
          floodVisible = entry.isIntersecting;
          if (floodVisible) floodRun();
        }
      });
    }, { threshold: 0.2 });
    io.observe(demo);
    if (floodScreen) io.observe(floodScreen);
  } else {
    reveal();
    demoVisible = true;
  }

  /* ---------- Theme preview (palettes from crates/noa-theme) ---------- */
  const THEMES = [{"name":"Noa","p":["#26262b","#d9725f","#9cc48a","#e8a33d","#7a9cc6","#b48ead","#7fb3b5","#c9cbc4","#82878f","#e8907f","#b5d6a5","#f0bc6a","#9db9dd","#cba8c4","#9fcbcc","#f7f7f2"],"bg":"#1b1d21","fg":"#f7f7f2","cur":"#e8a33d"},{"name":"Catppuccin Mocha","p":["#45475a","#f38ba8","#a6e3a1","#f9e2af","#89b4fa","#f5c2e7","#94e2d5","#bac2de","#585b70","#f7aec2","#c2ecbf","#fcd682","#aeccfc","#f398da","#b1eae1","#a6adc8"],"bg":"#1e1e2e","fg":"#cdd6f4","cur":"#f5e0dc"},{"name":"Dracula","p":["#21222c","#ff5555","#50fa7b","#f1fa8c","#bd93f9","#ff79c6","#8be9fd","#f8f8f2","#6272a4","#ff6e6e","#69ff94","#ffffa5","#d6acff","#ff92df","#a4ffff","#ffffff"],"bg":"#282a36","fg":"#f8f8f2","cur":"#f8f8f2"},{"name":"Gruvbox Dark","p":["#282828","#cc241d","#98971a","#d79921","#458588","#b16286","#689d6a","#a89984","#928374","#fb4934","#b8bb26","#fabd2f","#83a598","#d3869b","#8ec07c","#ebdbb2"],"bg":"#282828","fg":"#ebdbb2","cur":"#ebdbb2"},{"name":"Nord","p":["#3b4252","#bf616a","#a3be8c","#ebcb8b","#81a1c1","#b48ead","#88c0d0","#e5e9f0","#596377","#bf616a","#a3be8c","#ebcb8b","#81a1c1","#b48ead","#8fbcbb","#eceff4"],"bg":"#2e3440","fg":"#d8dee9","cur":"#eceff4"},{"name":"TokyoNight","p":["#15161e","#f7768e","#9ece6a","#e0af68","#7aa2f7","#bb9af7","#7dcfff","#a9b1d6","#414868","#f7768e","#9ece6a","#e0af68","#7aa2f7","#bb9af7","#7dcfff","#c0caf5"],"bg":"#1a1b26","fg":"#c0caf5","cur":"#c0caf5"},{"name":"Kanagawa Wave","p":["#090618","#c34043","#76946a","#c0a36e","#7e9cd8","#957fb8","#6a9589","#c8c093","#727169","#e82424","#98bb6c","#e6c384","#7fb4ca","#938aa9","#7aa89f","#dcd7ba"],"bg":"#1f1f28","fg":"#dcd7ba","cur":"#dcd7ba"},{"name":"Rose Pine Dawn","p":["#f2e9e1","#b4637a","#286983","#ea9d34","#56949f","#907aa9","#d7827e","#575279","#9893a5","#b4637a","#286983","#ea9d34","#56949f","#907aa9","#d7827e","#575279"],"bg":"#faf4ed","fg":"#575279","cur":"#575279"},{"name":"GitHub Light Default","p":["#24292f","#cf222e","#116329","#4d2d00","#0969da","#8250df","#1b7c83","#6e7781","#57606a","#a40e26","#1a7f37","#633c01","#218bff","#a475f9","#3192aa","#8c959f"],"bg":"#ffffff","fg":"#1f2328","cur":"#0969da"}];

  const swatches = $("#swatches");
  const term = $("#theme-term");
  const themeName = $("#theme-name");
  const palette = $("#palette");
  const swatchButtons = [];

  function applyTheme(i, focus) {
    const t = THEMES[i];
    term.style.setProperty("--th-bg", t.bg);
    term.style.setProperty("--th-fg", t.fg);
    term.style.setProperty("--th-cur", t.cur);
    [1, 2, 3, 4, 5, 6, 8].forEach((n) => term.style.setProperty("--th-" + n, t.p[n]));
    themeName.textContent = 'theme = "' + t.name + '"';
    palette.replaceChildren(...t.p.map((c) => { const el = document.createElement("i"); el.style.background = c; return el; }));
    swatchButtons.forEach((b, j) => {
      b.setAttribute("aria-checked", String(i === j));
      b.tabIndex = i === j ? 0 : -1;
    });
    if (focus) swatchButtons[i].focus();
  }

  THEMES.forEach((t, i) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "swatch";
    b.setAttribute("role", "radio");
    const dot = document.createElement("i");
    dot.style.background = "linear-gradient(135deg, " + t.bg + " 50%, " + t.p[3] + " 50%)";
    dot.setAttribute("aria-hidden", "true");
    b.append(dot, document.createTextNode(t.name));
    b.addEventListener("click", () => applyTheme(i, false));
    b.addEventListener("keydown", (e) => {
      const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
      if (step === undefined) return;
      e.preventDefault();
      applyTheme((i + step + THEMES.length) % THEMES.length, true);
    });
    swatchButtons.push(b);
    swatches.append(b);
  });
  applyTheme(0, false);
})();
