import { useEffect, useRef, useState, useId } from "react";
import Spoiler from "./Spoiler.jsx";
import MarkdownText from "./MarkdownText.jsx";
import Tooltip from "./Tooltip.jsx";

// Die silhouettes in a 0..100 viewBox. `points` is the outer polygon, `facets`
// are inner lines/triangles that suggest the polyhedron, `cy` centres the numeral.
// d6 is a real CSS cube (below); these drive d4/d8/d10/d12/d20 + a hex fallback.
const DICE = {
  4: { points: "50,9 91,84 9,84", facets: ["9,84 50,52 91,84", "50,9 50,52"], cy: 64, fs: 30 },
  8: { points: "50,7 91,50 50,93 9,50", facets: ["9,50 91,50", "50,7 50,93"], cy: 53, fs: 30 },
  10: { points: "50,5 89,39 50,96 11,39", facets: ["11,39 89,39", "50,5 50,39", "50,39 50,96"], cy: 47, fs: 26 },
  12: { points: "50,7 90,36 75,86 25,86 10,36", facets: ["28,40 72,40 65,72 35,72 28,40"], cy: 55, fs: 28 },
  20: { points: "50,5 87,27 87,73 50,95 13,73 13,27", facets: ["33,61 67,61 50,31 33,61", "13,27 33,61", "87,27 67,61", "13,73 33,61", "87,73 67,61", "50,95 50,61"], cy: 50, fs: 27 },
  hex: { points: "50,6 88,28 88,72 50,94 12,72 12,28", facets: ["32,50 68,50 50,20 32,50"], cy: 54, fs: 26 },
};

// Standard d6 pip layout (3x3 grid inside the face).
const PIPS = {
  1: [[50, 50]],
  2: [[34, 34], [66, 66]],
  3: [[34, 34], [50, 50], [66, 66]],
  4: [[34, 34], [66, 34], [34, 66], [66, 66]],
  5: [[34, 34], [66, 34], [50, 50], [34, 66], [66, 66]],
  6: [[34, 34], [66, 34], [34, 50], [66, 50], [34, 66], [66, 66]],
};

// Full grade ladder from world._grade_from_margin (+ attack crits, ungraded, invalid).
const GRADE = {
  overwhelming_success: { label: "разгромный успех", cls: "crit-ok" },
  critical_success: { label: "крит. успех", cls: "crit-ok" },
  strong_success: { label: "уверенный успех", cls: "ok" },
  success: { label: "успех", cls: "ok" },
  near_miss: { label: "почти удалось", cls: "fail" },
  weak_failure: { label: "слабый провал", cls: "fail" },
  failure: { label: "провал", cls: "fail" },
  major_failure: { label: "серьёзный провал", cls: "fail" },
  critical_failure: { label: "крит. провал", cls: "crit-fail" },
  ungraded: { label: "", cls: "neutral" },
  invalid: { label: "неверная формула", cls: "fail" },
};

const GRADE_ACCENT = {
  ok: "var(--gm)",
  "crit-ok": "var(--md-strong)",
  fail: "var(--redo)",
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

// d6 as a real cube: each value's face placed on the cube surface (half-edge 28px
// for the 56px die), plus the rotation that brings that value to the front
// (DeSandro show-face convention). Opposite faces sum to 7 — a real die.
const HALF = 28;
const CUBE_FACE = {
  1: `translateZ(${HALF}px)`,
  2: `rotateY(90deg) translateZ(${HALF}px)`,
  3: `rotateX(90deg) translateZ(${HALF}px)`,
  4: `rotateX(-90deg) translateZ(${HALF}px)`,
  5: `rotateY(-90deg) translateZ(${HALF}px)`,
  6: `rotateY(180deg) translateZ(${HALF}px)`,
};
const FACING = {
  1: [0, 0, 0],
  2: [0, -90, 0],
  3: [-90, 0, 0],
  4: [90, 0, 0],
  5: [0, 90, 0],
  6: [0, -180, 0],
};
const rotStr = ([rx, ry, rz]) => `rotateX(${rx}deg) rotateY(${ry}deg) rotateZ(${rz}deg)`;
const DROPPED_T = " translateZ(-16px) rotateX(14deg) scale(.9)";

// Animate-once guard: a roll tumbles the first time it streams in; on transcript
// re-mount (Virtuoso recycles rows on scroll) it snaps straight to the result.
const animatedRolls = new Set();

// One physical die. The 3D rotation lives ONLY on the inner `.die-spin` shell
// (kept clean of opacity/filter so preserve-3d never flattens); dropped opacity,
// crit classes, aura and contact shadow live on the outer non-3d nodes.
function Die({ sides, value, dropped, index, crit, rollKey, animate }) {
  const spinRef = useRef(null);
  const seenKey = rollKey != null ? `${rollKey}:${index}` : null;
  const snap = prefersReduced || !animate || (seenKey != null && animatedRolls.has(seenKey));
  const [landed, setLanded] = useState(snap);
  const target = sides === 6 ? FACING[value] || [0, 0, 0] : [0, 0, 0];
  const landTransform = rotStr(target) + (dropped ? DROPPED_T : "");
  // Snapped dice get their landed orientation inline so there is no first-frame flash;
  // animating dice leave transform to the WAAPI roll (React never sets it, so no clobber).
  const spinStyle = snap ? { transform: landTransform } : undefined;

  useEffect(() => {
    const el = spinRef.current;
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
    const spins = coarsePointer ? 2 : 3;
    const delay = index * 90; // index-derived stagger — never RNG (replay-stable)
    const timer = setTimeout(() => {
      if (cancelled || !spinRef.current) return;
      const k = 360 * spins;
      const [rx, ry, rz] = target;
      const from = el.style.transform || rotStr([0, 0, 0]);
      const spun =
        `rotateX(${rx + k}deg) rotateY(${ry - k}deg) rotateZ(${rz}deg)` + (dropped ? DROPPED_T : "");
      el.style.willChange = "transform";
      anim = el.animate(
        [{ transform: from }, { transform: spun }],
        { duration: 740, easing: "cubic-bezier(.2,.8,.25,1)", fill: "forwards" }
      );
      anim.finished
        .then(() => {
          if (cancelled) return;
          el.style.transform = landTransform; // strip the extra spins so the next roll's math can't drift
          el.style.willChange = "auto";
          setLanded(true);
        })
        .catch(() => {});
    }, delay);

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
  }, [sides, value, dropped, index, seenKey, animate]);

  const cls =
    "die" +
    (dropped ? " dropped" : "") +
    (landed ? " landed" : " rolling") +
    (landed && crit ? " " + crit : "");
  const tip =
    `d${sides}: выпало ${value}` +
    (dropped ? "\n(отброшено правилом keep)" : "") +
    (crit === "nat20" ? "\nнатуральная 20" : crit === "nat1" ? "\nнатуральная 1" : "");

  return (
    <Tooltip as="div" className={cls} style={{ "--i": index }} tipClassName="tool-tip" content={tip}>
      <span className="die-aura" aria-hidden="true" />
      <span className="die-scene">
        {sides === 6 ? (
          <span className="die-spin cube" ref={spinRef} style={spinStyle}>
            {[1, 2, 3, 4, 5, 6].map((n) => (
              <span className="cube-face" style={{ transform: CUBE_FACE[n] }} key={n}>
                <svg viewBox="0 0 100 100" className="cube-svg" aria-hidden="true">
                  {PIPS[n].map(([cx, cy], i) => (
                    <circle key={i} cx={cx} cy={cy} r="9" className="die-pip" />
                  ))}
                </svg>
              </span>
            ))}
          </span>
        ) : (
          <span className="die-spin shell" ref={spinRef} style={spinStyle}>
            <span className="shell-face front">
              <PolyFace sides={sides} value={value} />
            </span>
            <span className="shell-face back">
              <PolyFace sides={sides} value={value} back />
            </span>
          </span>
        )}
      </span>
      <span className="die-shadow" aria-hidden="true" />
    </Tooltip>
  );
}

// A faceted polyhedral face. 3D look comes from a per-instance radial gradient
// (lit top-left -> deep shadow) + facet lines + a gloss highlight — all static
// (computed once), so the only animated thing is the shell's transform.
function PolyFace({ sides, value, back }) {
  const uid = useId().replace(/[:]/g, "");
  const faceId = `f${uid}`;
  const glossId = `g${uid}`;
  const g = DICE[sides] || DICE.hex;
  return (
    <svg viewBox="0 0 100 100" className={"die-svg" + (back ? " back" : "")} aria-hidden="true">
      <defs>
        <radialGradient id={faceId} cx="34%" cy="28%" r="82%">
          <stop offset="0%" stopColor="#4a4127" />
          <stop offset="46%" stopColor="#23232f" />
          <stop offset="100%" stopColor="#0d0d16" />
        </radialGradient>
        <linearGradient id={glossId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#fff" stopOpacity=".26" />
          <stop offset="46%" stopColor="#fff" stopOpacity="0" />
        </linearGradient>
      </defs>
      <polygon points={g.points} fill={`url(#${faceId})`} className="die-face" />
      {g.facets.map((f, i) => (
        <polyline key={i} points={f} className="die-facet" />
      ))}
      <polygon points={g.points} fill={`url(#${glossId})`} className="die-gloss" />
      {!back && (
        <text
          x="50"
          y={g.cy}
          className="die-num"
          textAnchor="middle"
          dominantBaseline="central"
          style={{ fontSize: g.fs }}
        >
          {value}
        </text>
      )}
    </svg>
  );
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

  return (
    <div className={"dice-body " + grade.cls}>
      <div className="dice-stage">
        {dice.map((d) => (
          <Die
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
        <span className="tc-ico dice-ico">🎲</span>
        <span className="tc-title">Бросок кубика</span>
        {r.notation && <span className="tc-name">{r.notation}</span>}
      </div>
      <DiceBody roll={r} animate={animate} rollId={rollId} />
    </div>
  );
}
