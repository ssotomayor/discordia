/* A globe made of dots on a plain 2D canvas. Orthographic, so a point is drawn
   at its rotated (x, y) and hidden once z goes behind the limb. One instance
   per canvas id; Rust talks to it through window.dxGlobe.* and hears back
   through the sink handed to mount(). */
if (!window.dxGlobe) {
  window.dxGlobe = (function () {
    // 360x180 land bits, one per degree, row 0 = lat 89.5, col 0 = lon -179.5.
    // Rasterised from Natural Earth 110m land; ocean is simply not drawn.
    var MASK = atob("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADg/wEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADA/v8/AOD//z8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD+///D/////5//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAPAH/j/+//////8PAAAAgB0AAHgAAAAAAD8AAAAAAAAAAAAAAAAAAAAAAAAAAOD//wP8//////8BAACAfx4AAAAAAAAAAP4AAAAAAAAAAAAAAAAAAAAAAAA4+I73//n///////8AAAAAfwAAAAAAAAAAAAAfAAAAAAAAAAAAAAAAAAAAAOAQACDwP4D///////8BAAAAPAQAAAAAAAAAAAAYAAAAAAAAAAAAAAAAAAAAADzAgPP5H8D//////38AAAAAAAAAAAAA4AEAAAD+DwAAAAAAAAAAAAAAAAAAAIC/g7PfBwAA/v////8AAAAAAAAAAADgBwAAwP//PwAA4B8AAAAAAAAAAAAAAAAMAAD8AgAA+P////8AAAAAAAAAAABwAAAA/P//AwAAAAAAAAAAAAAAAAAAAP8Ahvc5OwAA8P///38AAAAAAAAAAAAcAADg////34cHAAcAAAAAAAAAAAAAgN9/xzf8RwAA4P///z8AAAAAAAAAAAAPAB7g//////8fgB8AAAAAAQAAAAAAAO//B3f8/wsA4P///z8AAAAAAAAAAAAPAG////////8f2f8HAAAAAAD4HwAAAAD8H/D4/38A4P///xMAAAAAAPAHAAAAgN//////////////DwAAAAD+///h/4//H+/Bg/8BwP7//w8AAAAAwP8fAAAAj9///////////////88/AeD///////8DB8TPB/4AAP///wAAAAAA8P//AwDjf77/////////////////DwD////////////vh/kPwP//BwAAAAAA/P//H/P//5//////////////////34H8////////////Afw/gP//AQAAAAAA/v+fH////+///////////////////vD///////////9fAPwYgP8PAOA/AAAA/+N/8P//////////////////////QID4///////////Pw/0DAP8HAMAfAACA//F//v////////////////////8/AAP4///////////BBPAHAP4HAAAAAADg//z///////////////////////9/AID///////////8AQ8ABAPwBAAAAAAD8P/7///////////////////////9fAMD//////////z8AwA8AAPgBAAAAAAD+H/7/////////////////////Of8BAID///z//////z8AwD8AAMABAAAAAAD+P/z///////////////////9/wH8AAAD8MwD//////x8AwH8MAAAAAAAAAAD+P2D///////////////////95cAAAAACAAwD8/////38AwP8eAAAAAAAAgAGcH/D//////////////////wEAOAAAAADADgDA/////38AgP8/AAAAAAAAwANAH/T//////////////////wAAfgAAAAAwAAAA//////8PgP8/AAAAAAAAwAFwD/7/////////////////PwAAfwAAAAAGAAAA//////8/wP//AAAAAAAAwAOwA/7/////////////////DwCAPwAAAIAAAAAA/v//////8///BwAAAAAAOAcgcP//////////////////HwAAHwAAAAAAAACA+P//////4///DwAAAAAAOA74/////////////////////wUADwAAAAAAAAAA8P//////4///DwAAAAAAGD///////////////////////wUAAwAAAAAAAAAA8P//////7///FwAAAAAAAB///////////////////////wUAAgAAAAAAAAAA0P//////////DAAAAAAAgOH//////////////////////w0AAAAAAAAAAAAAYP////////8xLAAAAAAAAPz//////////////////////wwAAAAAAAAAAAAAAP7//////38HfgAAAAAAgP///////////////////////wQAAAAAAAAAAAAAAP3///////8HUAAAAAAAAP7/////////////////////fwQAAAAAAAAAAAAAAP////////+XAAAAAAAAAPz///93/D/+////////////PwwAAAAAAAAAAAAAAP////////9/AAAAAAAAAPj//f9j/g/+////////////HwAAAAAAAAAAAAAAAP////////8MAAAAAAAAAPj/+P8B/Mf/////////////DwQAAAAAAAAAAAAAAP///////z8AAAAAAAAAcPzH8/8A8I//////////////Bx4AAAAAAAAAAAAAAP///////x8AAAAAAAAA+H+Aw/8AwA/+//////////9/AA8AAAAAAAAAAAAAAP///////x8AAAAAAAAA+D8Aj//w4R/8//////////8/AAAAAAAAAAAAAAAAAP///////wMAAAAAAAAA+B8wuE/+/z/+/////////98fAAMAAAAAAAAAAAAAAP///////wMAAAAAAAAA+A8wEMf//x/+/////////0cOAAMAAAAAAAAAAAAAAP7//////wEAAAAAAAAA+A8AAI7//x/8/////////wMOAAEAAAAAAAAAAAAAAP7//////wAAAAAAAAAA+AcABob//z/8/////////zccwAEAAAAAAAAAAAAAAPz//////wAAAAAAAAAAQOB/AAQ2/////////////x8c8AEAAAAAAAAAAAAAAPj//////wAAAAAAAAAAQPx/AAAA/////////////w8Y/gEAAAAAAAAAAAAAAPD/////fwAAAAAAAAAA4P8/AAAA/////////////w+EGwAAAAAAAAAAAAAAAMD/////HwAAAAAAAAAA8P9/AACA/////////////x/AAwAAAAAAAAAAAAAAAID/////DwAAAAAAAAAA+P//BwaA/////////////x/AAAAAAAAAAAAAAAAAAID9////BwAAAAAAAAAA/P//D3+E/////////////z9AAAAAAAAAAAAAAAAAAAD5////BwAAAAAAAAAA/P//f////////////////x8AAAAAAAAAAAAAAAAAAADy/x8GBgAAAAAAAAAA/P/////v/8///////////z8AAAAAAAAAAAAAAAAAAAD0/w8ABgAAAAAAAAAA/v////+//4///////////z8AAAAAAAAAAAAAAAAAAADu/wcADgAAAAAAAACA//////8f/x/+/////////x8AAAAAAAAAAAAAAAAAAACI/wcALAAAAAAAAADA//////8//z/g/////////w8AAAAAAAAAAAAAAAAAAACQ/wcACAAAAAAAAADg//////9//r+A/////////wcAAAAAAAAAAAAAAAAAAAAQ/wMAAAAAAAAAAADg//////9//n8cgP///////ycAAAAAAAAAAAAAAAAAAAAA/gMAAAAAAAAAAADw////////+P9/AP///////xEAAAAAAAAAAAAAAAAAAAAA/AMAHQAAAAAAAADw////////+P//APz/f///fxAAAAAAAAAAAAAAAAAAAAAA+AcAYAAAAAAAAAD4////////+f9/APz/B///BgAAAAAAAAAAAAAAAAAAAAAA/AccgAEAAAAAAADw////////8f9/AOD/B/5/AAAAAAAAAAAAAAAAAQAAAAAA+A8eADgAAAAAAADw////////4f8/AOD/Afw/BgAAAAAAAAAAAAAAAAAAAAAA8J8PAPwCAAAAAADw////////4/8fAOD/APw/AgAAAAAAAAAAAAAAAAAAAAAAgP8PAAAAAAAAAADw////////x/8HAOB/APx/ADAAAAAAAAAAAAAAAAAAAAAAAP4PAAAAAAAAAAD4////////h/8BAOA/APz/ADAAAAAAAAAAAAAAAAAAAAAAAID/AAAAAAAAAAD4////////j/8AAMAPAMD/ATAAAAAAAAAAAAAAAAAAAAAAAAD/AQAAAAAAAAD4////////nx8AAMAPAMD/ATAAAAAAAAAAAAAAAAAAAAAAAAD8AAAAAAAAAAD4////////vwcAAIAPAMD/AcAAAAAAAAAAAAAAAAAAAAAAAADgAQAAAAAAAAD4////////fwAAAIAPAMD8AQABAAAAAAAAAAAAAAAAAAAAAADAAAgAAAAAAADw////////f2AAAAAPAID4AUACAAAAAAAAAAAAAAAAAAAAAADAAe9TAAAAAADg/////////34AAAAPAEBwAAgAAAAAAAAAAAAAAAAAAAAAAAAAE+9/AAAAAADA/////////38AAAAXAEAgAAQCAAAAAAAAAAAAAAAAAAAAAAAAz///AAAAAACA/////////z8AAAASAMAAAIACAAAAAAAAAAAAAAAAAAAAAAAAyP//AQAAAACA/////////z8AAAAwAIAAAEAHAAAAAAAAAAAAAAAAAAAAAAAAgP//AwAAAAAA/v///////x8AAAAwAAADAAMBAAAAAAAAAAAAAAAAAAAAAAAAwP//fwAAAAAA/A/+/////x8AAAAAAAAHAAcAAAAAAAAAAAAAAAAAAAAAAAAAgP///wAAAAAAEAD8/////w8AAAAAADAGwAcAAAAAAAAAAAAAAAAAAAAAAAAAgP///wEAAAAAAADA/////wcAAAAAAGAG4AEAAAAAAAAAAAAAAAAAAAAAAAAA4P///wEAAAAAAADA/////wMAAAAAAMAM+AMAAAAAAAAAAAAAAAAAAAAAAAAA4P///wMAAAAAAADg/////wEAAAAAAIAL/gMQAAAAAAAAAAAAAAAAAAAAAAAA8P///wMAAAAAAADg////fwAAAAAAAIAH/vMQAAAAAAAAAAAAAAAAAAAAAAAA8P///wcAAAAAAADg////PwAAAAAAAAAP/gMAAQAAAAAAAAAAAAAAAAAAAAAA+P///38AAAAAAADg////PwAAAAAAAAAO/DmAAwAAAAAAAAAAAAAAAAAAAAAA+P///38BAAAAAADA////HwAAAAAAAAA+/DkA8gAAAAAAAAAAAAAAAAAAAAAA8P////8fAAAAAACA////DwAAAAAAAAA8wChE/gcAAAAAAAAAAAAAAAAAAAAA+P////8/AAAAAACA////BwAAAAAAAAA4AEAA8B8AAAAAAAAAAAAAAAAAAAAA+P//////AQAAAAAA////BwAAAAAAAAAwAEAAxD8NAAAAAAAAAAAAAAAAAAAA+P//////AQAAAAAA////BwAAAAAAAADAAQAAxP+AAAAAAAAAAAAAAAAAAAAA8P//////AQAAAAAA/v//BwAAAAAAAACAHwAAwH8ABAAAAAAAAAAAAAAAAAAA4P//////AQAAAAAA/v//BwAAAAAAAAAAQFYEAMcAAAAAAAAAAAAAAAAAAAAA4P//////AAAAAAAA/v//DwAAAAAAAAAAAAgBAIABMAAAAAAAAAAAAAAAAAAAwP//////AAAAAAAA/v//DwAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAgP////9/AAAAAAAA/P//DwAAAAAAAAAAAAAAAQQAAAAAAAAAAAAAAAAAAAAAgP////8/AAAAAAAA/v//HyAAAAAAAAAAAACAHwQAAAAAAAAAAAAAAAAAAAAAAP////8fAAAAAAAA/v//HyAAAAAAAAAAAADADwwAAAAAAAAAAAAAAAAAAAAAAP////8fAAAAAAAA////HzAAAAAAAAAAAADsDxwAAAAAAAAAAAAAAAAAAAAAAP7///8fAAAAAAAA////DzgAAAAAAAAAAAD/DxwAAAAAAAAAAAAAAAAAAAAAAPj///8fAAAAAAAA////Dz8AAAAAAAAAAAD/Pz4AAAiAAAAAAAAAAAAAAAAAAOD///8fAAAAAAAA////Ax8AAAAAAAAAAMD//z4AAABAAAAAAAAAAAAAAAAAAMD///8PAAAAAAAA////AB8AAAAAAAAAAMD//z8AAAAAAAAAAAAAAAAAAAAAAMD///8PAAAAAAAA/v9/AB8AAAAAAAAAAOD//38AAAAAAAAAAAAAAAAAAAAAAMD///8PAAAAAAAA/v9/AB8AAAAAAAAAAPz///8BAAEAAAAAAAAAAAAAAAAAAMD///8HAAAAAAAA/P9/gA8AAAAAAAAAgP////8BAAIAAAAAAAAAAAAAAAAAAMD///8DAAAAAAAA/P//gA8AAAAAAAAAwP////8DAAAAAAAAAAAAAAAAAAAAAMD//38AAAAAAAAA/P9/AA8AAAAAAAAAwP////8HAAAAAAAAAAAAAAAAAAAAAOD//x8AAAAAAAAA+P9/AAcAAAAAAAAA4P////8PAAAAAAAAAAAAAAAAAAAAAOD//w8AAAAAAAAA+P8fAAIAAAAAAAAAwP////8fAAAAAAAAAAAAAAAAAAAAAOD//wcAAAAAAAAA+P8fAAAAAAAAAAAA4P////8fAAAAAAAAAAAAAAAAAAAAAOD//wcAAAAAAAAA+P8fAAAAAAAAAAAAwP////8fAAAAAAAAAAAAAAAAAAAAAOD//wcAAAAAAAAA8P8PAAAAAAAAAAAAgP////8/AAAAAAAAAAAAAAAAAAAAAOD//wMAAAAAAAAA4P8HAAAAAAAAAAAAgP////8fAAAAAAAAAAAAAAAAAAAAAPD//wMAAAAAAAAA4P8HAAAAAAAAAAAAgP////8fAAAAAAAAAAAAAAAAAAAAAPD//wEAAAAAAAAAwP8DAAAAAAAAAAAAAP////8fAAAAAAAAAAAAAAAAAAAAAOD//wAAAAAAAAAAwP8BAAAAAAAAAAAAAP8D/P8PAAAAAAAAAAAAAAAAAAAAAPD/fwAAAAAAAAAAwH8AAAAAAAAAAAAAgP8A2P8HAAAAAAAAAAAAAAAAAAAAAPD/OwAAAAAAAAAAgAEAAAAAAAAAAAAAAAcA6P8HAAAAAAAAAAAAAAAAAAAAAPj/BwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAwP8DAAACAAAAAAAAAAAAAAAAAPj/BwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP8DAAAEAAAAAAAAAAAAAAAAAPz/BwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP8DAAAIAAAAAAAAAAAAAAAAAPj/AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAGgAAAA4AAAAAAAAAAAAAAAAAPg/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAcAAAAAAAAAAAAAAAAAPw/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAYAAAAAAAAAAAAAAAAAPwHAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAOAAAAALAAAAAAAAAAAAAAAAAPwfAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAOAAAIADAAAAAAAAAAAAAAAAAPgHAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEAAAMABAAAAAAAAAAAAAAAAAPwHAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHAAAAAAAAAAAAAAAAAAAP4BAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHgAAAAAAAAAAAAAAAAAAP4BAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADAAAAAAAAAAAAAAAAAAAP4DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP8BAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP8AAAAAAAAAAAAAAAAAAAAAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAH4AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAH6AAwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAPwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAPADAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAOAAAAAAAAAAAAAAAAAIAfAAAAAAAeeADADwAAAAAAAAAAAAAAAAAAAAAAAAAHAAAAAAAAAAAAAAAAAPD/AADA/////////z8AAAAAAAAAAAAAAAAAAAAAAAAHAAAAAAAAAAAAAAAAgP///wH4//////////8DAAAAAAAAAAAAAAAAAAAAADAfAAAAAAAAAAAAAADw8////wP+////////////AwAAAAAAAAAAAAAAAAAAAHA/AAAAAAAAAAA8//P//////+D/////////////HwAAAAAAAAAAAAAAAAAAAP5+AAAAAAAA5v////////////D//////////////38AAAAAAAAAAAAAAAAAAAB+AAAAAACA/////////////////////////////38AAAAAAAAAYAYA4ON/OPh/AAAAAADw/////////////////////////////x8AAAAAAADwv/MXgP////8fAAAAAADw/////////////////////////////wMAAAAAAPz///////////8HAAAAAAD+/////////////////////////////wAAAAAAAPz//////////z8AAAAAAP///////////////////////////////wAAAADA/////////////wEAAAAA8P///////////////////////////////wAAAACH////////////PwAAAPgA/////////////////////////////////wcAAAAcgP//////////fwAAAP4BwP//////////////////////////////HwAAAAAAAP7//////////wf8wD8AwP//////////////////////////////DwAAAAAA/v////////////8HAAD4////////////////////////////////HwAAAAAA+P//////////////4f///////////////////////////////////wAAAAAA/P///////////////////////////////////////////////////x8A3x8AAP7///////////////////////////////////////////////////8/////f///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////");
    function land(lat, lon) {
      var row = Math.min(179, Math.max(0, Math.floor(90 - lat)));
      var col = Math.min(359, Math.max(0, Math.floor(lon + 180)));
      var i = row * 360 + col;
      return (MASK.charCodeAt(i >> 3) >> (i & 7)) & 1;
    }

    var TAU = Math.PI * 2;
    var instances = {};
    var pending = {};
    var reduceMotion = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    function wrap(a) { return a - TAU * Math.floor((a + Math.PI) / TAU); }

    function makeDots(r) {
      var n = Math.min(9000, Math.max(1500, Math.round(r * r * 0.3)));
      var golden = Math.PI * (3 - Math.sqrt(5));
      var out = [];
      for (var i = 0; i < n; i++) {
        var y = 1 - (2 * i + 1) / n;
        var rad = Math.sqrt(1 - y * y);
        var th = golden * i;
        var x = Math.cos(th) * rad, z = Math.sin(th) * rad;
        var lat = Math.asin(y) * 180 / Math.PI;
        var lon = Math.atan2(-z, x) * 180 / Math.PI;
        if (land(lat, lon)) out.push({ x: x, y: y, z: z });
      }
      return out;
    }

    // z points at the viewer, so east has to run to the right: -sin(lon).
    function unit(lat, lon) {
      var la = lat * Math.PI / 180, lo = lon * Math.PI / 180;
      return { x: Math.cos(la) * Math.cos(lo), y: Math.sin(la), z: -Math.cos(la) * Math.sin(lo) };
    }

    // Yaw about the vertical axis, then pitch about the screen's horizontal.
    function project(g, p) {
      var cy = Math.cos(g.yaw), sy = Math.sin(g.yaw), cp = Math.cos(g.pitch), sp = Math.sin(g.pitch);
      var x1 = p.x * cy + p.z * sy;
      var z1 = -p.x * sy + p.z * cy;
      var y2 = p.y * cp - z1 * sp;
      var z2 = p.y * sp + z1 * cp;
      return { sx: g.cx + g.r * x1, sy: g.cy - g.r * y2, z: z2 };
    }

    function unproject(g, sx, sy) {
      var x = (sx - g.cx) / g.r, y = -(sy - g.cy) / g.r;
      var d = x * x + y * y;
      if (d > 1) return null;
      var z = Math.sqrt(1 - d);
      var cp = Math.cos(g.pitch), sp = Math.sin(g.pitch), cy = Math.cos(g.yaw), sy = Math.sin(g.yaw);
      var y0 = y * cp + z * sp;
      var z1 = -y * sp + z * cp;
      var x0 = x * cy - z1 * sy;
      var z0 = x * sy + z1 * cy;
      return { lat: Math.asin(Math.max(-1, Math.min(1, y0))) * 180 / Math.PI, lon: Math.atan2(-z0, x0) * 180 / Math.PI };
    }

    function colors(g) {
      var cs = getComputedStyle(g.canvas);
      var get = function (name, fallback) { var v = cs.getPropertyValue(name).trim(); return v || fallback; };
      g.col = {
        accent: get('--accent', '#8fb0ff'),
        dot: get('--text-dim', '#8a8478'),
        muted: get('--text-muted', '#b0a898'),
        text: get('--text', '#f0ebe2'),
        panel: get('--panel-solid', '#17150f'),
        edge: get('--edge-strong', '#322c24')
      };
    }

    function resize(g) {
      var rect = g.canvas.getBoundingClientRect();
      var dpr = window.devicePixelRatio || 1;
      var w = Math.max(1, Math.round(rect.width)), h = Math.max(1, Math.round(rect.height));
      if (g.w === w && g.h === h && g.dpr === dpr) return;
      g.w = w; g.h = h; g.dpr = dpr;
      g.canvas.width = Math.round(w * dpr);
      g.canvas.height = Math.round(h * dpr);
      g.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      g.cx = w / 2; g.cy = h / 2;
      g.r = Math.max(20, Math.min(w, h) / 2 - 14);
      g.dots = makeDots(g.r);
    }

    function pinAt(g, sx, sy) {
      var best = null, bestD = 12 * 12;
      for (var i = 0; i < g.pins.length; i++) {
        var pr = project(g, g.pins[i].u);
        if (pr.z < 0) continue;
        var dx = pr.sx - sx, dy = pr.sy - sy, d = dx * dx + dy * dy;
        if (d < bestD) { bestD = d; best = g.pins[i]; }
      }
      return best;
    }

    function label(g, text, x, y, strong) {
      var ctx = g.ctx;
      ctx.font = (strong ? '600 ' : '500 ') + '11px system-ui, sans-serif';
      var w = ctx.measureText(text).width + 14, h = 20;
      var lx = Math.min(g.w - w - 2, Math.max(2, x - w / 2));
      var ly = y - 14 - h;
      if (ly < 2) ly = y + 14;
      ctx.globalAlpha = 0.96;
      ctx.fillStyle = g.col.panel;
      ctx.strokeStyle = g.col.edge;
      ctx.lineWidth = 1;
      ctx.beginPath();
      if (ctx.roundRect) ctx.roundRect(lx, ly, w, h, 6); else ctx.rect(lx, ly, w, h);
      ctx.fill(); ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.fillStyle = strong ? g.col.text : g.col.muted;
      ctx.textBaseline = 'middle';
      ctx.fillText(text, lx + 7, ly + h / 2 + 0.5);
    }

    function draw(g, now) {
      var ctx = g.ctx;
      ctx.clearRect(0, 0, g.w, g.h);

      ctx.globalAlpha = 0.06;
      ctx.fillStyle = g.col.text;
      ctx.beginPath(); ctx.arc(g.cx, g.cy, g.r + 1, 0, TAU); ctx.fill();
      ctx.globalAlpha = 0.35;
      ctx.strokeStyle = g.col.edge;
      ctx.lineWidth = 1;
      ctx.beginPath(); ctx.arc(g.cx, g.cy, g.r + 4, 0, TAU); ctx.stroke();

      // Six depth buckets: one path each instead of one arc each.
      var buckets = [[], [], [], [], [], []];
      for (var i = 0; i < g.dots.length; i++) {
        var pr = project(g, g.dots[i]);
        if (pr.z <= 0) continue;
        buckets[Math.min(5, Math.floor(pr.z * 6))].push(pr);
      }
      ctx.fillStyle = g.col.dot;
      var scale = Math.max(0.8, g.r / 130);
      for (var b = 0; b < 6; b++) {
        var list = buckets[b];
        if (!list.length) continue;
        var zc = (b + 0.5) / 6;
        ctx.globalAlpha = 0.18 + 0.62 * zc;
        var rad = (0.9 + 0.9 * zc) * scale;
        ctx.beginPath();
        for (var k = 0; k < list.length; k++) {
          ctx.moveTo(list[k].sx + rad, list[k].sy);
          ctx.arc(list[k].sx, list[k].sy, rad, 0, TAU);
        }
        ctx.fill();
      }

      var pulse = 0.5 + 0.5 * Math.sin(now / 600);
      var hoverTip = null, selTip = null;
      for (var p = 0; p < g.pins.length; p++) {
        var pin = g.pins[p];
        var q = project(g, pin.u);
        if (q.z < -0.08) continue;
        var fade = q.z < 0 ? (q.z + 0.08) / 0.08 : 1;
        var selected = pin.code === g.selected;
        var hovered = pin === g.hover;
        var col = pin.fresh ? g.col.accent : g.col.muted;
        var base = selected ? 5 : 3.5;
        if (pin.fresh || selected) {
          ctx.globalAlpha = fade * (0.35 - 0.25 * pulse);
          ctx.strokeStyle = col; ctx.lineWidth = 1.5;
          ctx.beginPath(); ctx.arc(q.sx, q.sy, base + 4 + 6 * pulse, 0, TAU); ctx.stroke();
        }
        ctx.globalAlpha = fade;
        ctx.fillStyle = g.col.panel;
        ctx.beginPath(); ctx.arc(q.sx, q.sy, base + 2, 0, TAU); ctx.fill();
        ctx.fillStyle = col;
        ctx.beginPath(); ctx.arc(q.sx, q.sy, base, 0, TAU); ctx.fill();
        if (selected && q.z > 0) selTip = { text: pin.label, x: q.sx, y: q.sy };
        else if (hovered && q.z > 0) hoverTip = { text: pin.label, x: q.sx, y: q.sy };
      }
      if (g.place) {
        var pq = project(g, g.place.u);
        if (pq.z > -0.08) {
          ctx.globalAlpha = pq.z < 0 ? (pq.z + 0.08) / 0.08 : 1;
          ctx.strokeStyle = g.col.accent; ctx.lineWidth = 1.5;
          ctx.beginPath(); ctx.arc(pq.sx, pq.sy, 9 + 3 * pulse, 0, TAU); ctx.stroke();
          ctx.fillStyle = g.col.accent;
          ctx.beginPath(); ctx.arc(pq.sx, pq.sy, 4, 0, TAU); ctx.fill();
        }
      }
      if (hoverTip) label(g, hoverTip.text, hoverTip.x, hoverTip.y, false);
      if (selTip) label(g, selTip.text, selTip.x, selTip.y, true);
      ctx.globalAlpha = 1;
    }

    function step(g, now) {
      if (!g.alive) return;
      resize(g);
      if ((g.frame++ % 30) === 0) colors(g);
      var dt = Math.min(64, now - (g.last || now)); g.last = now;

      if (g.target) {
        var dy = wrap(g.target.yaw - g.yaw), dp = g.target.pitch - g.pitch;
        g.yaw += dy * 0.1; g.pitch += dp * 0.1;
        if (Math.abs(dy) < 0.002 && Math.abs(dp) < 0.002) g.target = null;
      } else if (!g.dragging) {
        g.yaw += g.vel; g.vel *= 0.94;
        if (Math.abs(g.vel) < 0.0002) g.vel = 0;
        var idle = now - g.touched > 2500 && !g.hover && !g.selected && !g.place;
        if (idle && !reduceMotion) g.yaw += 0.00012 * dt;
      }
      g.pitch = Math.max(-1.25, Math.min(1.25, g.pitch));
      g.yaw = wrap(g.yaw);
      draw(g, now);
      g.raf = requestAnimationFrame(function (t) { step(g, t); });
    }

    function wire(g) {
      var c = g.canvas;
      c.addEventListener('pointerdown', function (e) {
        g.dragging = true; g.moved = 0; g.target = null; g.vel = 0;
        g.px = e.clientX; g.py = e.clientY; g.touched = performance.now();
        c.setPointerCapture(e.pointerId);
        c.style.cursor = 'grabbing';
      });
      c.addEventListener('pointermove', function (e) {
        var rect = c.getBoundingClientRect();
        var sx = e.clientX - rect.left, sy = e.clientY - rect.top;
        if (g.dragging) {
          var dx = e.clientX - g.px, dy = e.clientY - g.py;
          g.px = e.clientX; g.py = e.clientY;
          g.moved += Math.abs(dx) + Math.abs(dy);
          g.yaw += dx / g.r; g.pitch += dy / g.r;
          g.vel = dx / g.r * 0.6;
          g.touched = performance.now();
          return;
        }
        var hit = pinAt(g, sx, sy);
        if (hit !== g.hover) { g.hover = hit; g.touched = performance.now(); }
        c.style.cursor = hit ? 'pointer' : (g.pick ? 'crosshair' : 'grab');
      });
      var end = function (e) {
        if (!g.dragging) return;
        g.dragging = false;
        c.style.cursor = g.pick ? 'crosshair' : 'grab';
        g.touched = performance.now();
        if (g.moved > 4) return;
        var rect = c.getBoundingClientRect();
        var sx = e.clientX - rect.left, sy = e.clientY - rect.top;
        var hit = pinAt(g, sx, sy);
        if (hit) { g.sink({ __dxf: 'globe-pick', id: g.id, code: hit.code }); return; }
        if (g.pick) {
          var ll = unproject(g, sx, sy);
          if (ll) g.sink({ __dxf: 'globe-place', id: g.id, lat: ll.lat, lon: ll.lon });
        }
      };
      c.addEventListener('pointerup', end);
      c.addEventListener('pointercancel', function () { g.dragging = false; });
      c.addEventListener('pointerleave', function () { if (!g.dragging) g.hover = null; });
    }

    function get(id) { return instances[id]; }

    function applyPins(g, pins) {
      g.pins = (pins || []).map(function (p) {
        return { code: p.code, label: p.label, fresh: !!p.fresh, u: unit(p.lat, p.lon), lat: p.lat, lon: p.lon };
      });
      if (g.hover && g.pins.indexOf(g.hover) === -1) g.hover = null;
    }

    function focusSelected(g) {
      for (var i = 0; i < g.pins.length; i++) {
        if (g.pins[i].code === g.selected) { focusOn(g, g.pins[i].lat, g.pins[i].lon); return; }
      }
    }

    function focusOn(g, lat, lon) {
      g.target = { yaw: -Math.PI / 2 - lon * Math.PI / 180, pitch: lat * Math.PI / 180 };
      g.touched = performance.now();
    }

    return {
      mount: function (id, sink) {
        var canvas = document.getElementById(id);
        if (!canvas || instances[id]) return;
        var g = {
          id: id, canvas: canvas, ctx: canvas.getContext('2d'), sink: sink,
          alive: true, frame: 0, dots: [], pins: [], selected: null, place: null, pick: false,
          yaw: -1.2, pitch: 0.35, vel: 0, target: null, dragging: false, hover: null,
          touched: 0, w: 0, h: 0, dpr: 0, r: 0, cx: 0, cy: 0, col: {}
        };
        instances[id] = g;
        wire(g);
        colors(g);
        var p = pending[id]; delete pending[id];
        if (p) { if (p.pins) applyPins(g, p.pins); if (p.selected !== undefined) g.selected = p.selected; if (p.pick !== undefined) g.pick = p.pick; if (p.place !== undefined) g.place = p.place; }
        if (g.place) focusOn(g, g.place.lat, g.place.lon);
        else if (g.selected) focusSelected(g);
        g.raf = requestAnimationFrame(function (t) { step(g, t); });
      },
      setPins: function (id, pins) {
        var g = get(id);
        if (!g) { (pending[id] = pending[id] || {}).pins = pins; return; }
        applyPins(g, pins);
      },
      setSelected: function (id, code) {
        var g = get(id);
        if (!g) { (pending[id] = pending[id] || {}).selected = code; return; }
        if (code === g.selected) return;
        g.selected = code;
        focusSelected(g);
      },
      setPick: function (id, pick) {
        var g = get(id);
        if (!g) { (pending[id] = pending[id] || {}).pick = pick; return; }
        g.pick = pick;
        g.canvas.style.cursor = pick ? 'crosshair' : 'grab';
      },
      setPlace: function (id, lat, lon) {
        var place = (lat === null || lat === undefined) ? null : { u: unit(lat, lon), lat: lat, lon: lon };
        var g = get(id);
        if (!g) { (pending[id] = pending[id] || {}).place = place; return; }
        g.place = place;
        if (place) focusOn(g, lat, lon);
      },
      destroy: function (id) {
        var g = get(id);
        delete pending[id];
        if (!g) return;
        g.alive = false;
        if (g.raf) cancelAnimationFrame(g.raf);
        delete instances[id];
      }
    };
  })();
}
