import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "./api.js";
import { useTranslation } from "react-i18next";

const EMPTY_RESULT = { items: [], total: 0, hasMore: false };

// One search lifecycle for all search surfaces. The effect is keyed by the
// serialized request rather than the caller's object identity, aborts the old
// request on every change and also keeps a sequence guard for environments
// where an already-resolved fetch cannot be cancelled in time.
export default function useAsyncSearch(params, { enabled = true, delay = 220 } = {}) {
  const { i18n, t } = useTranslation("game");
  const requestKey = JSON.stringify(params || {});
  const request = useMemo(() => JSON.parse(requestKey), [requestKey]);
  const sequenceRef = useRef(0);
  const abortRef = useRef(null);
  const [result, setResult] = useState(EMPTY_RESULT);
  const [initialLoading, setInitialLoading] = useState(false);
  const [revalidating, setRevalidating] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const sequence = ++sequenceRef.current;
    abortRef.current?.abort();
    abortRef.current = null;

    if (!enabled) {
      setResult(EMPTY_RESULT);
      setInitialLoading(false);
      setRevalidating(false);
      setError("");
      return undefined;
    }

    if (result.items.length === 0) setInitialLoading(true);
    else setRevalidating(true);
    setError("");

    const timer = window.setTimeout(async () => {
      const controller = new AbortController();
      abortRef.current = controller;
      try {
        const data = await api.search(request, { signal: controller.signal });
        if (!data?.ok) throw new Error(t("search.unavailable"));
        if (controller.signal.aborted || sequence !== sequenceRef.current) return;
        setResult({
          items: Array.isArray(data.items) ? data.items : [],
          total: Number.isFinite(Number(data.total)) ? Number(data.total) : 0,
          hasMore: Boolean(data.has_more),
        });
        setError("");
      } catch {
        if (controller.signal.aborted || sequence !== sequenceRef.current) return;
        setError(t("search.unavailable"));
      } finally {
        if (!controller.signal.aborted && sequence === sequenceRef.current) {
          setInitialLoading(false);
          setRevalidating(false);
        }
      }
    }, Math.max(0, Number(delay) || 0));

    return () => {
      window.clearTimeout(timer);
      if (abortRef.current) abortRef.current.abort();
    };
    // `result.items.length` intentionally stays out: retaining prior results is
    // presentation state and must not restart the network request.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requestKey, enabled, delay, i18n.language, t]);

  return {
    ...result,
    initialLoading,
    revalidating,
    error,
  };
}
