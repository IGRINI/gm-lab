import { Fragment, useContext } from "react";
import Tooltip from "./Tooltip.jsx";
import { EntityRegistryContext, canonicalKind, resolveEntity } from "../entityContext.js";

const SAFE_LINK_RE = /^(https?:|mailto:)/i;
const ENTITY_KIND_LABELS = {
  npc: "персонаж",
  loc: "локация",
  item: "предмет",
  note: "заметка",
};

function parseEntityRef(part) {
  const match = String(part || "").match(/^\[\[([a-z][a-z0-9_-]*):([^\]|\n]+)(?:\|([^\]\n]+))?\]\]$/i);
  if (!match) return null;
  return {
    kind: canonicalKind(match[1]),
    id: match[2].trim(),
    label: (match[3] || match[2]).trim(),
  };
}

function entityColor(kind, entity) {
  // Color is data-driven: a per-entity color from the card wins; otherwise a
  // centralized theme token per kind. No name-based color table.
  if (entity?.color) return entity.color;
  if (kind === "loc") return "var(--entity-loc)";
  if (kind === "item") return "var(--entity-item)";
  if (kind === "note") return "var(--entity-note)";
  return "var(--entity-unknown)";
}

function EntityTooltip({ entity, kind, id, label }) {
  const typeLabel = ENTITY_KIND_LABELS[kind] || kind || "сущность";
  const title = entity?.title || entity?.name || label || id;
  const subtitle = entity?.subtitle || typeLabel;
  const description = entity?.description || entity?.text || "";
  const meta = Array.isArray(entity?.meta) ? entity.meta : [];
  return (
    <div className="entity-tip">
      <div className="entity-tip-head">
        <span>{typeLabel}</span>
        <b style={{ color: entityColor(kind, entity) }}>{title}</b>
        {subtitle && <em>{subtitle}</em>}
      </div>
      {description && <div className="entity-tip-desc">{description}</div>}
      {meta.length > 0 && (
        <div className="entity-tip-meta">
          {meta.map((row, idx) => (
            <div key={idx}>
              <span>{row.label}</span>
              <b>{row.value}</b>
            </div>
          ))}
        </div>
      )}
      {!entity && <div className="entity-tip-desc muted">Нет данных в текущем реестре.</div>}
    </div>
  );
}

function EntityRef({ token, registry, keyPrefix }) {
  const ref = parseEntityRef(token);
  if (!ref) return token;
  const entity = resolveEntity(registry, ref.kind, ref.id);
  const label = ref.label || entity?.label || entity?.name || ref.id;
  return (
    <Tooltip
      className={["entity-ref", `entity-ref-${ref.kind}`, entity ? "" : "missing"].filter(Boolean).join(" ")}
      tipClassName="entity-tip-wrap"
      content={<EntityTooltip entity={entity} kind={ref.kind} id={ref.id} label={label} />}
    >
      <span style={{ "--entity-color": entityColor(ref.kind, entity) }}>
        {inlineNodes(label, `${keyPrefix}-label`, registry, { autoEntities: false })}
      </span>
    </Tooltip>
  );
}

const WORD_CHAR_RE = /[\p{L}\p{N}_]/u;

function isWordChar(ch) {
  return Boolean(ch && WORD_CHAR_RE.test(ch));
}

function hasTextBoundary(src, start, end) {
  return !isWordChar(src[start - 1]) && !isWordChar(src[end]);
}

function entityLabel(entity) {
  return String(entity?.label || entity?.title || entity?.name || "").trim();
}

function addUniqueName(rows, value) {
  const clean = String(value || "").trim();
  if (clean) rows.push(clean);
}

function addRussianNameForms(rows, name) {
  const clean = String(name || "").trim();
  if (!/[а-яё]/i.test(clean) || clean.length < 3) return;
  const lower = clean.toLocaleLowerCase("ru-RU");
  if (/[ая]$/iu.test(lower)) {
    const stem = clean.slice(0, -1);
    addUniqueName(rows, `${stem}ы`);
    addUniqueName(rows, `${stem}е`);
    addUniqueName(rows, `${stem}у`);
    addUniqueName(rows, `${stem}ой`);
    return;
  }
  if (/ий$/iu.test(lower)) {
    const stem = clean.slice(0, -2);
    addUniqueName(rows, `${stem}ия`);
    addUniqueName(rows, `${stem}ию`);
    addUniqueName(rows, `${stem}ием`);
    return;
  }
  if (/й$/iu.test(lower)) {
    const stem = clean.slice(0, -1);
    addUniqueName(rows, `${stem}я`);
    addUniqueName(rows, `${stem}ю`);
    addUniqueName(rows, `${stem}ем`);
    return;
  }
  if (/ь$/iu.test(lower)) {
    const stem = clean.slice(0, -1);
    addUniqueName(rows, `${stem}я`);
    addUniqueName(rows, `${stem}ю`);
    addUniqueName(rows, `${stem}ем`);
    addUniqueName(rows, `${stem}и`);
    return;
  }
  addUniqueName(rows, `${clean}а`);
  addUniqueName(rows, `${clean}у`);
  addUniqueName(rows, `${clean}ом`);
  addUniqueName(rows, `${clean}е`);
}

function entityAliases(entity) {
  const names = [entityLabel(entity), entity?.title, entity?.name, ...(Array.isArray(entity?.aliases) ? entity.aliases : [])]
    .map((value) => String(value || "").trim())
    .filter(Boolean);
  const main = names[0] || "";
  const words = main.match(/[\p{L}\p{N}_-]+/gu) || [];
  if (words.length > 1) {
    const last = words[words.length - 1];
    if (last && last.length >= 3) names.push(last);
  }
  for (const name of [...names]) addRussianNameForms(names, name);
  return names;
}

function autoEntityCandidates(registry) {
  const byKey = registry?.byKey || {};
  const seen = new Set();
  const rows = [];
  for (const entity of Object.values(byKey)) {
    const kind = canonicalKind(entity?.kind || entity?.type);
    if (kind !== "npc") continue;
    for (const label of entityAliases(entity)) {
      if (label.length < 3) continue;
      const dedupeKey = `${kind}:${label.toLocaleLowerCase("ru-RU")}`;
      if (seen.has(dedupeKey)) continue;
      seen.add(dedupeKey);
      rows.push({ entity, kind, label, needle: label.toLocaleLowerCase("ru-RU") });
    }
  }
  return rows.sort((a, b) => b.label.length - a.label.length);
}

function findNextEntityMatch(line, start, candidates) {
  const haystack = line.toLocaleLowerCase("ru-RU");
  let best = null;
  for (const candidate of candidates) {
    let from = start;
    while (from < line.length) {
      const index = haystack.indexOf(candidate.needle, from);
      if (index === -1) break;
      const end = index + candidate.label.length;
      if (hasTextBoundary(line, index, end)) {
        if (!best || index < best.index || (index === best.index && candidate.label.length > best.candidate.label.length)) {
          best = { index, end, candidate };
        }
        break;
      }
      from = index + 1;
    }
  }
  return best;
}

function AutoEntityRef({ entity, kind, id, label }) {
  return (
    <Tooltip
      className={["entity-ref", `entity-ref-${kind}`].filter(Boolean).join(" ")}
      tipClassName="entity-tip-wrap"
      content={<EntityTooltip entity={entity} kind={kind} id={id} label={label} />}
    >
      <span style={{ "--entity-color": entityColor(kind, entity) }}>{label}</span>
    </Tooltip>
  );
}

function textNodesWithEntities(text, keyPrefix, registry, options) {
  const candidates = options.autoEntities ? autoEntityCandidates(registry) : [];
  return String(text ?? "").split("\n").map((piece, lineIndex) => {
    const nodes = [];
    let pos = 0;
    if (candidates.length) {
      while (pos < piece.length) {
        const match = findNextEntityMatch(piece, pos, candidates);
        if (!match) break;
        if (match.index > pos) nodes.push(piece.slice(pos, match.index));
        const label = piece.slice(match.index, match.end);
        const entity = match.candidate.entity;
        nodes.push(
          <AutoEntityRef
            key={`${keyPrefix}-${lineIndex}-e-${nodes.length}`}
            entity={entity}
            kind={match.candidate.kind}
            id={entity?.id}
            label={label}
          />
        );
        pos = match.end;
      }
    }
    if (pos < piece.length || !nodes.length) nodes.push(piece.slice(pos));
    return (
      <Fragment key={`${keyPrefix}-${lineIndex}`}>
        {lineIndex > 0 ? <br /> : null}
        {nodes}
      </Fragment>
    );
  });
}

function splitInline(text) {
  const src = String(text ?? "");
  const parts = [];
  const re = /(`[^`]+`|\[\[[a-z][a-z0-9_-]*:[^\]|\n]+(?:\|[^\]\n]+)?\]\]|\*\*[\s\S]+?\*\*|__[\s\S]+?__|~~[\s\S]+?~~|\*[^*\n]+?\*|_[^_\n]+?_|!\[[^\]]*]\([^)]+\)|\[[^\]]+]\([^)]+\))/gi;
  let last = 0;
  let match;
  while ((match = re.exec(src))) {
    if (match.index > last) parts.push(src.slice(last, match.index));
    parts.push(match[0]);
    last = re.lastIndex;
  }
  if (last < src.length) parts.push(src.slice(last));
  return parts;
}

function inlineNodes(text, keyPrefix = "i", registry = null, options = {}) {
  const opts = { autoEntities: true, ...options };
  return splitInline(text).map((part, idx) => {
    const key = `${keyPrefix}-${idx}`;
    if (!part) return null;

    if (part.startsWith("`") && part.endsWith("`")) {
      return <code key={key}>{part.slice(1, -1)}</code>;
    }
    if (part.startsWith("[[")) {
      return <EntityRef key={key} token={part} registry={registry} keyPrefix={key} />;
    }
    if ((part.startsWith("**") && part.endsWith("**")) || (part.startsWith("__") && part.endsWith("__"))) {
      return <strong key={key}>{inlineNodes(part.slice(2, -2), key, registry, opts)}</strong>;
    }
    if (part.startsWith("~~") && part.endsWith("~~")) {
      return <del key={key}>{inlineNodes(part.slice(2, -2), key, registry, opts)}</del>;
    }
    if ((part.startsWith("*") && part.endsWith("*")) || (part.startsWith("_") && part.endsWith("_"))) {
      return <em key={key}>{inlineNodes(part.slice(1, -1), key, registry, opts)}</em>;
    }
    if (part.startsWith("![") || part.startsWith("[")) {
      const image = part.startsWith("!");
      const offset = image ? 1 : 0;
      const close = part.indexOf("]", offset);
      const label = part.slice(offset + 1, close);
      const href = part.slice(close + 2, -1).trim();
      if (image) return <span key={key}>{label || href}</span>;
      if (!SAFE_LINK_RE.test(href)) return <span key={key}>{part}</span>;
      return (
        <a key={key} href={href} target="_blank" rel="noreferrer">
          {inlineNodes(label, key, registry, { ...opts, autoEntities: false })}
        </a>
      );
    }

    return textNodesWithEntities(part, key, registry, opts);
  });
}

function isFence(line) {
  return line.trim().startsWith("```");
}

function isBlockStart(line) {
  return (
    !line.trim() ||
    isFence(line) ||
    /^#{1,6}\s+/.test(line) ||
    /^\s*([-*+])\s+/.test(line) ||
    /^\s*\d+\.\s+/.test(line) ||
    /^\s*>\s?/.test(line)
  );
}

function takeList(lines, start, ordered) {
  const items = [];
  let i = start;
  const re = ordered ? /^\s*\d+\.\s+(.+)$/ : /^\s*[-*+]\s+(.+)$/;
  while (i < lines.length) {
    const match = lines[i].match(re);
    if (!match) break;
    items.push(match[1]);
    i += 1;
  }
  return [items, i];
}

function renderBlocks(text, registry = null) {
  const lines = String(text ?? "").replace(/\r\n/g, "\n").split("\n");
  const out = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i += 1;
      continue;
    }

    if (isFence(line)) {
      const lang = line.trim().slice(3).trim();
      const code = [];
      i += 1;
      while (i < lines.length && !isFence(lines[i])) {
        code.push(lines[i]);
        i += 1;
      }
      if (i < lines.length) i += 1;
      out.push(
        <pre key={`b-${out.length}`} className={lang ? `language-${lang}` : undefined}>
          <code>{code.join("\n")}</code>
        </pre>
      );
      continue;
    }

    const heading = line.match(/^(#{1,6})\s+(.+)$/);
    if (heading) {
      const level = Math.min(6, heading[1].length);
      const Tag = `h${level}`;
      out.push(<Tag key={`b-${out.length}`}>{inlineNodes(heading[2], `h-${out.length}`, registry)}</Tag>);
      i += 1;
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const quote = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        quote.push(lines[i].replace(/^\s*>\s?/, ""));
        i += 1;
      }
      out.push(<blockquote key={`b-${out.length}`}>{renderBlocks(quote.join("\n"), registry)}</blockquote>);
      continue;
    }

    if (/^\s*\d+\.\s+/.test(line)) {
      const [items, next] = takeList(lines, i, true);
      out.push(
        <ol key={`b-${out.length}`}>
          {items.map((item, idx) => <li key={idx}>{inlineNodes(item, `ol-${out.length}-${idx}`, registry)}</li>)}
        </ol>
      );
      i = next;
      continue;
    }

    if (/^\s*[-*+]\s+/.test(line)) {
      const [items, next] = takeList(lines, i, false);
      out.push(
        <ul key={`b-${out.length}`}>
          {items.map((item, idx) => <li key={idx}>{inlineNodes(item, `ul-${out.length}-${idx}`, registry)}</li>)}
        </ul>
      );
      i = next;
      continue;
    }

    const paragraph = [line];
    i += 1;
    while (i < lines.length && !isBlockStart(lines[i])) {
      paragraph.push(lines[i]);
      i += 1;
    }
    out.push(<p key={`b-${out.length}`}>{inlineNodes(paragraph.join("\n"), `p-${out.length}`, registry)}</p>);
  }

  return out.length ? out : "—";
}

export function MarkdownInline({ children }) {
  const registry = useContext(EntityRegistryContext);
  return <>{inlineNodes(children || "—", "i", registry)}</>;
}

export default function MarkdownText({ children, className = "" }) {
  const registry = useContext(EntityRegistryContext);
  return <div className={["md", className].filter(Boolean).join(" ")}>{renderBlocks(children || "—", registry)}</div>;
}
