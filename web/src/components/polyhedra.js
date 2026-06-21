// Real 3D dice geometry for d4/d8/d10/d12/d20.
//
// Faces are derived from exact vertices by a tiny convex-hull pass, so face
// planarity is guaranteed and we never hand-maintain (error-prone) face lists.
// Each face yields: a CSS `matrix3d` that places a flat face element on the solid,
// a local polygon (in px) drawn as an SVG, a baked flat-shade, and a die value.
// `facing(value)` returns the Euler [x,y,z] that rotates that face to the camera,
// so the shell can tumble-and-land deterministically (same as the d6 cube).

const PHI = (1 + Math.sqrt(5)) / 2;
const IP = 1 / PHI;

const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const addv = (a, b) => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const vlen = (a) => Math.hypot(a[0], a[1], a[2]);
const sclv = (a, s) => [a[0] * s, a[1] * s, a[2] * s];
const nrmv = (a) => {
  const l = vlen(a) || 1;
  return [a[0] / l, a[1] / l, a[2] / l];
};
const clamp = (x, lo, hi) => Math.max(lo, Math.min(hi, x));

// --- vertex sets (any circumradius; normalized later) ---
function tetra() {
  return [[1, 1, 1], [1, -1, -1], [-1, 1, -1], [-1, -1, 1]];
}
function octa() {
  return [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
}
function cube() {
  const v = [];
  for (const x of [1, -1]) for (const y of [1, -1]) for (const z of [1, -1]) v.push([x, y, z]);
  return v;
}
function icosa() {
  return [
    [0, 1, PHI], [0, 1, -PHI], [0, -1, PHI], [0, -1, -PHI],
    [1, PHI, 0], [1, -PHI, 0], [-1, PHI, 0], [-1, -PHI, 0],
    [PHI, 0, 1], [PHI, 0, -1], [-PHI, 0, 1], [-PHI, 0, -1],
  ];
}
function dodeca() {
  const v = [];
  for (const sx of [1, -1]) for (const sy of [1, -1]) for (const sz of [1, -1]) v.push([sx, sy, sz]);
  for (const s1 of [1, -1]) for (const s2 of [1, -1]) {
    v.push([0, s1 * IP, s2 * PHI]);
    v.push([s1 * IP, s2 * PHI, 0]);
    v.push([s1 * PHI, 0, s2 * IP]);
  }
  return v;
}
// d10 = pentagonal trapezohedron, built as the polar dual of a pentagonal
// antiprism (dual faces are planar by construction -> clean 10 kite faces).
function trapezohedron10() {
  const H = 0.62; // antiprism half-height -> d10 elongation
  const ap = [];
  for (let i = 0; i < 5; i++) {
    const a = (Math.PI * 2 * i) / 5;
    ap.push([Math.cos(a), Math.sin(a), H]);
  }
  for (let i = 0; i < 5; i++) {
    const a = (Math.PI * 2 * i) / 5 + Math.PI / 5;
    ap.push([Math.cos(a), Math.sin(a), -H]);
  }
  const faces = [[0, 1, 2, 3, 4], [5, 6, 7, 8, 9]];
  for (let i = 0; i < 5; i++) {
    const t0 = i, t1 = (i + 1) % 5, b0 = 5 + i, b1 = 5 + ((i + 1) % 5);
    faces.push([t0, t1, b0]);
    faces.push([b0, b1, t1]);
  }
  // polar pole of each antiprism face = a trapezohedron vertex
  return faces.map((f) => {
    const c = sclv(f.reduce((acc, idx) => addv(acc, ap[idx]), [0, 0, 0]), 1 / f.length);
    let n = nrmv(cross(sub(ap[f[1]], ap[f[0]]), sub(ap[f[2]], ap[f[0]])));
    if (dot(n, c) < 0) n = sclv(n, -1);
    const d = dot(n, ap[f[0]]);
    return sclv(n, 1 / d); // pole n/d
  });
}

const SOLID_VERTS = { 4: tetra, 6: cube, 8: octa, 10: trapezohedron10, 12: dodeca, 20: icosa };

// Convex-hull faces from a vertex cloud: every plane through 3 vertices that keeps
// all other vertices on one side is a hull face. Coplanar vertices are grouped and
// ordered CCW around the face centroid.
function hullFaces(verts) {
  const n = verts.length;
  const eps = 1e-4;
  const planes = new Map();
  for (let i = 0; i < n; i++)
    for (let j = i + 1; j < n; j++)
      for (let k = j + 1; k < n; k++) {
        let nrm = cross(sub(verts[j], verts[i]), sub(verts[k], verts[i]));
        if (vlen(nrm) < eps) continue;
        nrm = nrmv(nrm);
        let d = dot(nrm, verts[i]);
        let pos = 0, neg = 0;
        for (let m = 0; m < n; m++) {
          const dist = dot(nrm, verts[m]) - d;
          if (dist > eps) pos++;
          else if (dist < -eps) neg++;
        }
        if (pos > 0 && neg > 0) continue; // verts straddle the plane -> not a hull face
        if (pos > 0) { nrm = sclv(nrm, -1); d = -d; } // orient normal outward
        const key = nrm.map((x) => x.toFixed(3)).join(",") + "|" + d.toFixed(3);
        if (!planes.has(key)) planes.set(key, { normal: nrm, d });
      }

  const faces = [];
  const seen = new Set();
  for (const { normal, d } of planes.values()) {
    const idx = [];
    for (let m = 0; m < n; m++) if (Math.abs(dot(normal, verts[m]) - d) < 1e-3) idx.push(m);
    if (idx.length < 3) continue;
    // Float error on near-duplicate planes (e.g. golden-ratio coords) can list the
    // same face twice; dedupe by its vertex-index set so a pentagon stays one face.
    const sig = idx.slice().sort((a, b) => a - b).join(",");
    if (seen.has(sig)) continue;
    seen.add(sig);
    const centroid = sclv(idx.reduce((a, m) => addv(a, verts[m]), [0, 0, 0]), 1 / idx.length);
    const ex = nrmv(sub(verts[idx[0]], centroid));
    const ey = nrmv(cross(normal, ex));
    idx.sort((a, b) => {
      const va = sub(verts[a], centroid), vb = sub(verts[b], centroid);
      return Math.atan2(dot(va, ey), dot(va, ex)) - Math.atan2(dot(vb, ey), dot(vb, ex));
    });
    faces.push({ idx, normal, centroid });
  }
  return faces;
}

// Euler [x,y,z] (deg) for CSS `rotateX(x) rotateY(y) rotateZ(z)` whose matrix Rx·Ry·Rz
// brings the given face (basis ex/ey/normal) to face the camera (+Z). R = transpose
// of [ex ey normal], i.e. rows ex, ey, normal.
function facingEuler(ex, ey, nrm) {
  const R = [ex, ey, nrm]; // rows
  const sy = clamp(R[0][2], -1, 1);
  const y = Math.asin(sy);
  let x, z;
  if (Math.abs(Math.cos(y)) > 1e-4) {
    x = Math.atan2(-R[1][2], R[2][2]);
    z = Math.atan2(-R[0][1], R[0][0]);
  } else {
    x = Math.atan2(R[1][0], R[1][1]);
    z = 0;
  }
  const deg = (r) => (r * 180) / Math.PI;
  return [deg(x), deg(y), deg(z)];
}

// Assign 1..N so opposite faces sum to N+1 where the solid has antipodal faces.
function assignValues(faces, N) {
  const used = new Array(faces.length).fill(false);
  let v = 1;
  for (let i = 0; i < faces.length; i++) {
    if (used[i]) continue;
    let opp = -1, best = 2;
    for (let j = 0; j < faces.length; j++) {
      if (used[j] || j === i) continue;
      const dt = dot(faces[i].normal, faces[j].normal);
      if (dt < best) { best = dt; opp = j; }
    }
    faces[i].value = v;
    used[i] = true;
    if (opp >= 0) { faces[opp].value = N + 1 - v; used[opp] = true; }
    v++;
  }
}

const LIGHT = nrmv([0.32, -0.55, 0.78]); // fixed light, top-left-front
const cache = new Map();

export function buildSolid(sides, R = 25) {
  const ck = `${sides}:${R}`;
  if (cache.has(ck)) return cache.get(ck);
  const gen = SOLID_VERTS[sides];
  if (!gen) return null;
  const raw = gen();
  const maxr = Math.max(...raw.map(vlen));
  const verts = raw.map((v) => sclv(v, R / maxr));
  const hull = hullFaces(verts);
  assignValues(hull, sides);

  const faces = hull.map((f) => {
    const { centroid: c, normal: nrm, idx } = f;
    const ex = nrmv(sub(verts[idx[0]], c));
    const ey = nrmv(cross(nrm, ex));
    const pts = idx.map((m) => {
      const r = sub(verts[m], c);
      return [dot(r, ex), dot(r, ey)];
    });
    // column-major matrix3d: columns ex, ey, nrm, then translation c
    const mtx = [
      ex[0], ex[1], ex[2], 0,
      ey[0], ey[1], ey[2], 0,
      nrm[0], nrm[1], nrm[2], 0,
      c[0], c[1], c[2], 1,
    ];
    const shade = clamp(0.5 + 0.5 * dot(nrm, LIGHT), 0.12, 1);
    return { value: f.value, pts, mtx, shade, euler: facingEuler(ex, ey, nrm) };
  });

  const byValue = new Map(faces.map((f) => [f.value, f]));
  const solid = {
    faces,
    facing(value) {
      const f = byValue.get(value) || faces[0];
      return f.euler;
    },
  };
  cache.set(ck, solid);
  return solid;
}

export const POLY_SIDES = new Set([4, 6, 8, 10, 12, 20]);
