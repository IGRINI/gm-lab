import Icon from "./Icon.jsx";
import { useEffect, useRef, useState } from "react";
import Spoiler from "./Spoiler.jsx";
import MarkdownText from "./MarkdownText.jsx";
import Tooltip from "./Tooltip.jsx";
import { buildSolid, POLY_SIDES } from "./polyhedra.js";

// Standard d6 pip layout as a 3x3 grid in {-1,0,1} units, laid out on the face's
// own edge axes so the pips sit square inside the (hull-derived) cube face.
const PIP_GRID = {
  1: [[0, 0]],
  2: [[-1, -1], [1, 1]],
  3: [[-1, -1], [0, 0], [1, 1]],
  4: [[-1, -1], [1, -1], [-1, 1], [1, 1]],
  5: [[-1, -1], [1, -1], [0, 0], [-1, 1], [1, 1]],
  6: [[-1, -1], [1, -1], [-1, 0], [1, 0], [-1, 1], [1, 1]],
};
// Pip positions in the face's local px coords, from its square corner list.
function cubeFacePips(pts, value) {
  const e = [pts[1][0] - pts[0][0], pts[1][1] - pts[0][1]]; // an edge direction
  const l = Math.hypot(e[0], e[1]) || 1;
  const u = [e[0] / l, e[1] / l];
  const v = [-u[1], u[0]];
  const Rsq = Math.hypot(pts[0][0], pts[0][1]); // corner distance
  const off = 0.3 * Rsq;
  const r = 0.155 * Rsq;
  return (PIP_GRID[value] || []).map(([gx, gy]) => ({
    cx: gx * off * u[0] + gy * off * v[0],
    cy: gx * off * u[1] + gy * off * v[1],
    r,
  }));
}

// Full grade ladder from world._grade_from_margin (+ attack crits, ungraded, invalid).
// A clear best→worst spectrum, so the badge reads as a graded outcome, not a binary.
const GRADE = {
  overwhelming_success: { label: "сокрушительный успех", cls: "crit-ok" },
  critical_success: { label: "критический успех", cls: "crit-ok" },
  strong_success: { label: "уверенный успех", cls: "ok" },
  success: { label: "успех", cls: "ok" },
  near_miss: { label: "почти получилось", cls: "fail" },
  weak_failure: { label: "лёгкая неудача", cls: "fail" },
  failure: { label: "провал", cls: "fail" },
  major_failure: { label: "тяжёлый провал", cls: "fail" },
  critical_failure: { label: "критический провал", cls: "crit-fail" },
  ungraded: { label: "", cls: "neutral" },
  invalid: { label: "неверная формула", cls: "fail" },
};

const GRADE_ACCENT = {
  ok: "var(--gm)",
  "crit-ok": "var(--md-strong)",
  fail: "var(--danger)",
  "crit-fail": "#d9485f",
  neutral: "var(--md-strong)",
};
export function gradeAccent(gradeKey) {
  const g = GRADE[gradeKey] || GRADE.ungraded;
  return GRADE_ACCENT[g.cls] || "var(--md-strong)";
}

const mql = (q) =>
  typeof window !== "undefined" && window.matchMedia && window.matchMedia(q).matches;
const prefersReduced = mql("(prefers-reduced-motion: reduce)");
const coarsePointer = mql("(pointer: coarse)");

// Circumradius (px) per die. A face-on cube shows only its front face, so d6 gets a
// slightly larger radius to read at a comparable size to the other solids.
const polyR = (sides) => (sides === 6 ? 27 : 25);
const NUM_FS = { 4: 19, 8: 16, 10: 15, 12: 14, 20: 12 };

const rotStr = ([rx, ry, rz]) => `rotateX(${rx}deg) rotateY(${ry}deg) rotateZ(${rz}deg)`;
const DROPPED_T = " translateZ(-16px) rotateX(14deg) scale(.9)";

// Roll timing: ONE continuous spin that decelerates smoothly onto the face over ~3s.
// A single ease-out means velocity only ever decreases — no "almost stop then jerk".
const ROLL_MS = 3000; // total roll duration (ms)
const ROLL_EASE = "cubic-bezier(.16,.6,.28,1)"; // fast start, smooth monotonic spin-down

// Baked flat-shade (0.12..1) -> a fill from deep shadow to lit parchment-gold.
function shadeFill(s) {
  const lerp = (a, b) => Math.round(a + (b - a) * s);
  return `rgb(${lerp(0x1a, 0xd9)},${lerp(0x19, 0xc0)},${lerp(0x24, 0x86)})`;
}

// Animate-once guard: a roll tumbles the first time it streams in; on transcript
// re-mount (Virtuoso recycles rows on scroll) it snaps straight to the result.
const animatedRolls = new Set();

const seenKeyOf = (rollKey, index) => (rollKey != null ? `${rollKey}:${index}` : null);
const isSnap = (rollKey, index, animate) => {
  const k = seenKeyOf(rollKey, index);
  return prefersReduced || !animate || (k != null && animatedRolls.has(k));
};

// Shared wrapper: crit/dropped state classes, a glow aura, the perspective scene, and
// the contact shadow — all on non-3D nodes so the inner shell's preserve-3d is safe.
function DieFrame({ index, dropped, crit, landed, tip, children }) {
  const cls =
    "die" +
    (dropped ? " dropped" : "") +
    (landed ? " landed" : " rolling") +
    (landed && crit ? " " + crit : "");
  return (
    <Tooltip as="div" className={cls} style={{ "--i": index }} tipClassName="tool-tip" content={tip}>
      <span className="die-aura" aria-hidden="true" />
      <span className="die-scene">{children}</span>
      <span className="die-shadow" aria-hidden="true" />
    </Tooltip>
  );
}

// Drives a WAAPI tumble on `ref` from a neutral start to the facing plus whole extra
// turns, landing exactly on the backend face. `keyframes(el, k)` returns
// [fromTransform, toTransform]; the bare facing (`landTransform`) is what the die
// settles on inline. Shared by the solids/coin/d3.
function useTumble(ref, { snap, seenKey, landTransform, keyframes, index, deps }) {
  const [landed, setLanded] = useState(snap);
  useEffect(() => {
    const el = ref.current;
    if (!el) return undefined;
    if (snap) {
      el.style.transform = landTransform;
      setLanded(true);
      return undefined;
    }
    if (seenKey != null) animatedRolls.add(seenKey);
    setLanded(false);
    let cancelled = false;
    let anim = null;
    const k = 360 * (coarsePointer ? 5 : 6); // total turns — fast at the start, eased to rest
    const timer = setTimeout(() => {
      if (cancelled || !ref.current) return;
      // One smooth spin that decelerates straight onto the face — no two-phase boundary,
      // so there's no re-acceleration / jerk at the end.
      const [from, to] = keyframes(el, k);
      el.style.willChange = "transform";
      anim = el.animate(
        [{ transform: from }, { transform: to }],
        { duration: ROLL_MS, easing: ROLL_EASE, fill: "none" }
      );
      // The to-keyframe carries the extra turns; settle on the bare facing inline so
      // the next roll can't accumulate drift (visually identical — whole turns only).
      el.style.transform = landTransform;
      anim.finished
        .then(() => {
          if (cancelled) return;
          el.style.willChange = "auto";
          setLanded(true);
        })
        .catch(() => {});
    }, index * 90);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      if (anim) {
        try {
          anim.cancel();
        } catch {
          /* already finished */
        }
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return landed;
}

// Real polyhedra (d4/d6/d8/d10/d12/d20) + a flat token fallback for odd sizes (d7…).
function Die({ sides, value, dropped, index, crit, rollKey, animate }) {
  const spinRef = useRef(null);
  const solid = POLY_SIDES.has(sides) ? buildSolid(sides, polyR(sides)) : null;
  const seenKey = seenKeyOf(rollKey, index);
  const snap = isSnap(rollKey, index, animate);
  const target = solid ? solid.facing(value) : [0, 0, 0];
  const droppedT = dropped ? DROPPED_T : "";
  const landTransform = rotStr(target) + droppedT;
  const landed = useTumble(spinRef, {
    snap,
    seenKey,
    landTransform,
    index,
    keyframes: (el, k) => {
      if (!solid) return ["rotateZ(0deg)" + droppedT, `rotateZ(${k}deg)` + droppedT]; // token: flat in-plane spin
      const [rx, ry, rz] = target;
      const from = (el.style.transform || rotStr([0, 0, 0])) ;
      const to = `rotateX(${rx + k}deg) rotateY(${ry - k}deg) rotateZ(${rz}deg)` + droppedT;
      return [from, to];
    },
    deps: [sides, value, dropped, index, seenKey, animate],
  });
  const spinStyle = snap ? { transform: landTransform } : undefined;
  const tip =
    `d${sides}: выпало ${value}` +
    (dropped ? "\n(отброшено правилом keep)" : "") +
    (crit === "nat20" ? "\nнатуральная 20" : crit === "nat1" ? "\nнатуральная 1" : "");

  return (
    <DieFrame index={index} dropped={dropped} crit={crit} landed={landed} tip={tip}>
      {!solid ? (
        <span className="die-spin token" ref={spinRef} style={spinStyle}>
          <svg viewBox="0 0 100 100" className="token-svg">
            <rect x="8" y="8" width="84" height="84" rx="18" className="poly-fill" style={{ fill: "#23232f" }} />
            <text x="50" y="50" className="poly-num" textAnchor="middle" dominantBaseline="central" style={{ fontSize: value > 99 ? 24 : 30 }}>
              {value}
            </text>
          </svg>
        </span>
      ) : (
        <span className="die-spin solid" ref={spinRef} style={spinStyle}>
          {solid.faces.map((f, i) => (
            <span className="poly-face" style={{ transform: `matrix3d(${f.mtx.join(",")})` }} key={i}>
              <svg viewBox="-40 -40 80 80" className="poly-svg">
                <polygon
                  points={f.pts.map((p) => `${p[0].toFixed(2)},${p[1].toFixed(2)}`).join(" ")}
                  className="poly-fill"
                  style={{ fill: shadeFill(f.shade) }}
                />
                {sides === 6 ? (
                  cubeFacePips(f.pts, f.value).map((p, j) => (
                    <circle key={j} cx={p.cx.toFixed(2)} cy={p.cy.toFixed(2)} r={p.r.toFixed(2)} className="die-pip" />
                  ))
                ) : (
                  <text
                    x="0"
                    y="0"
                    className="poly-num"
                    textAnchor="middle"
                    dominantBaseline="central"
                    style={{ fontSize: NUM_FS[sides] || 14 }}
                  >
                    {f.value}
                  </text>
                )}
              </svg>
            </span>
          ))}
        </span>
      )}
    </DieFrame>
  );
}

// d2 — a real coin: two faces (1/2) along Z + a cylindrical rim, flipping end-over-end
// (rotateX) and landing with the rolled face toward the camera.
const COIN_R = 24;
const COIN_SEG = Array.from({ length: 18 }, (_, i) => (360 / 18) * i);
function CoinDie({ value, index, rollKey, animate }) {
  const spinRef = useRef(null);
  const seenKey = seenKeyOf(rollKey, index);
  const snap = isSnap(rollKey, index, animate);
  const ry = value === 2 ? 180 : 0;
  const land = `rotateX(0deg) rotateY(${ry}deg)`;
  const landed = useTumble(spinRef, {
    snap,
    seenKey,
    landTransform: land,
    index,
    keyframes: (el, k) => ["rotateX(0deg) rotateY(0deg)", `rotateX(${k}deg) rotateY(${ry}deg)`],
    deps: [value, index, seenKey, animate],
  });
  return (
    <DieFrame index={index} landed={landed} tip={`d2 (монета): выпало ${value}`}>
      <span className="die-spin coin" ref={spinRef} style={snap ? { transform: land } : undefined}>
        <span className="coin-face coin-front" style={{ transform: "translateZ(4px)" }}>1</span>
        <span className="coin-face coin-back" style={{ transform: "rotateY(180deg) translateZ(4px)" }}>2</span>
        {COIN_SEG.map((a, i) => (
          <span className="coin-edge" key={i} style={{ transform: `rotateZ(${a}deg) translateX(${COIN_R}px) rotateY(90deg)` }} />
        ))}
      </span>
    </DieFrame>
  );
}

// d3 — a sphere with three flat faces 120° apart around the VERTICAL axis. The value's
// number sits on the rounded ridge facing the camera dead-centre; the two flats flank
// it left and right, the third flat hides at the back (the one it "rests" on). Numbers
// live on the ridges between flats. Spins about Y to land.
const D3_R = 24, D3_FLAT_R = 16;
function D3Die({ value, index, rollKey, animate }) {
  const spinRef = useRef(null);
  const seenKey = seenKeyOf(rollKey, index);
  const snap = isSnap(rollKey, index, animate);
  const ry = -120 * (value - 1); // brings the value's ridge to front-centre
  const land = `rotateY(${ry}deg)`;
  const landed = useTumble(spinRef, {
    snap,
    seenKey,
    landTransform: land,
    index,
    keyframes: (el, k) => ["rotateY(0deg)", `rotateY(${ry - k}deg)`],
    deps: [value, index, seenKey, animate],
  });
  return (
    <DieFrame index={index} landed={landed} tip={`d3 (шар с 3 гранями): выпало ${value}`}>
      <span className="d3-body" />
      <span className="die-spin d3" ref={spinRef} style={snap ? { transform: land } : undefined}>
        {[1, 2, 3].map((n) => (
          <span className="d3-flat" key={"f" + n} style={{ transform: `rotateY(${60 + 120 * (n - 1)}deg) translateZ(${D3_FLAT_R}px)` }} />
        ))}
        {[1, 2, 3].map((n) => (
          <span className="d3-num" key={"n" + n} style={{ transform: `rotateY(${120 * (n - 1)}deg) translateZ(${D3_R}px)` }}>
            {n}
          </span>
        ))}
      </span>
    </DieFrame>
  );
}

// d100 — a magic 8-ball: a glossy black sphere with a window where the number tumbles
// through random values, then settles on the backend roll with a pop.
function BallDie({ value, index, rollKey, animate }) {
  const snap = isSnap(rollKey, index, animate);
  const seenKey = seenKeyOf(rollKey, index);
  const [num, setNum] = useState(value);
  const [landed, setLanded] = useState(snap);
  useEffect(() => {
    if (snap) {
      setNum(value);
      setLanded(true);
      return undefined;
    }
    if (seenKey != null) animatedRolls.add(seenKey);
    setLanded(false);
    let cancelled = false;
    let raf = 0;
    let last = 0;
    const start = performance.now();
    const dur = ROLL_MS + index * 90;
    const tick = (now) => {
      if (cancelled) return;
      const t = now - start;
      if (t >= dur) {
        setNum(value);
        setLanded(true);
        return;
      }
      // window randomizes 1..100 (final value = backend roll, set above); the cycle
      // slows like a slot machine toward the end so the reveal has anticipation.
      const p = t / dur;
      const interval = 55 + 200 * p * p;
      if (now - last > interval) {
        setNum(1 + Math.floor(Math.random() * 100));
        last = now;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value, index, seenKey, animate]);
  return (
    <DieFrame index={index} landed={landed} tip={`d100 (шар-предсказание): выпало ${value}`}>
      <span className={"ball" + (landed ? " landed" : " rolling")}>
        <span className="ball-body" />
        <span className="ball-eight">8</span>
        <span className="ball-window">
          <span className="ball-num" style={{ fontSize: num > 99 ? 13 : 16 }}>{num}</span>
        </span>
        <span className="ball-gloss" />
      </span>
    </DieFrame>
  );
}

// Picks the right visual per die size.
function DieFor(props) {
  if (props.sides === 2) return <CoinDie {...props} />;
  if (props.sides === 3) return <D3Die {...props} />;
  if (props.sides === 100) return <BallDie {...props} />;
  return <Die {...props} />;
}

// Maps each rolled face to a Die, marking dropped (not in `kept`) and crit faces.
function diceList(roll) {
  const sides = Number(roll.sides) || 20;
  const rolls = Array.isArray(roll.rolls) && roll.rolls.length ? roll.rolls : [Number(roll.total) || 0];
  const kept = Array.isArray(roll.kept) ? roll.kept.slice() : rolls.slice();
  const keptPool = kept.slice();
  const single = sides === 20 && kept.length === 1;
  return rolls.map((v, idx) => {
    const ki = keptPool.indexOf(v);
    const dropped = ki === -1;
    if (!dropped) keptPool.splice(ki, 1);
    let crit = "";
    if (single && !dropped) {
      if (v === 20) crit = "nat20";
      else if (v === 1) crit = "nat1";
    }
    return { sides, value: v, dropped, crit, key: idx };
  });
}

// The dice visuals (stage + readout + detail) without any card chrome, so it can
// live inside a merged ToolCard or the standalone DiceRoll card below.
// `animate` gates the tumble (live roll) vs snap (restored history); `rollId` is the
// stable timeline message id used to animate each roll exactly once.
export function DiceBody({ roll, animate = true, rollId }) {
  const r = roll || {};
  const dice = diceList(r);
  const grade = GRADE[r.grade] || GRADE.ungraded;
  const mod = Number(r.modifier) || 0;
  const kept = Array.isArray(r.kept) ? r.kept : [];
  const hasTarget = r.target_number != null && r.roll_kind && r.roll_kind !== "roll";

  const showFormula = kept.length > 1 || mod !== 0;
  const formula =
    kept.join(" + ") + (mod ? ` ${mod > 0 ? "+" : "−"} ${Math.abs(mod)}` : "") + ` = ${r.total}`;

  // GM-supplied reason for a modifier / advantage, shown beside the die as
  // "+N от: <причина>". keep "kh"/"kl" carries advantage/disadvantage with no number.
  const keep = String(r.keep || "");
  const note = typeof r.modifier_note === "string" ? r.modifier_note.trim() : "";
  const modAmt = mod
    ? `${mod > 0 ? "+" : "−"}${Math.abs(mod)}`
    : keep.startsWith("kh")
    ? "преимущество"
    : keep.startsWith("kl")
    ? "помеха"
    : "";
  const negativeMod = mod < 0 || keep.startsWith("kl");

  return (
    <div className={"dice-body " + grade.cls}>
      <div className="dice-stage">
        {dice.map((d) => (
          <DieFor
            key={d.key}
            sides={d.sides}
            value={d.value}
            dropped={d.dropped}
            index={d.key}
            crit={d.crit}
            rollKey={rollId}
            animate={animate}
          />
        ))}
        {note && (
          <div className={"dice-modnote" + (negativeMod ? " neg" : "")}>
            {modAmt && <b className="dice-modnote-amt">{modAmt}</b>}
            <span className="dice-modnote-reason">{modAmt ? " от: " : ""}{note}</span>
          </div>
        )}
      </div>

      <div className="dice-readout">
        {showFormula && <span className="dice-formula">{formula}</span>}
        <span className="dice-total" aria-label={`итог ${r.total}`}>{r.total}</span>
        {grade.label && <span className={"dice-grade " + grade.cls}>{grade.label}</span>}
        {hasTarget && (
          <span className="dice-target">
            {(r.target_kind || "DC")} {r.target_number}
            {r.margin != null && <em className="dice-margin">{r.margin >= 0 ? `+${r.margin}` : r.margin}</em>}
          </span>
        )}
      </div>

      {r.detail && (
        <Spoiler label="детали броска">
          <MarkdownText>{"```\n" + r.detail + "\n```"}</MarkdownText>
        </Spoiler>
      )}
    </div>
  );
}

// Standalone dice card — fallback for a `dice` result with no matching tool call.
export default function DiceRoll({ roll, animate, rollId }) {
  const r = roll || {};
  const grade = GRADE[r.grade] || GRADE.ungraded;
  return (
    <div className={"tool-card dice-card " + grade.cls}>
      <div className="tc-hd">
        <span className="tc-ico dice-ico"><Icon name="d20" size={15} /></span>
        <span className="tc-title">Бросок кубика</span>
        {r.notation && <span className="tc-name">{r.notation}</span>}
      </div>
      <DiceBody roll={r} animate={animate} rollId={rollId} />
    </div>
  );
}
