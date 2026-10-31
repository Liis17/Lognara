import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, errorText } from "./api";
import type { InfrastructureResponse } from "./types";

const REFRESH_MS = 10_000;

export function useInfrastructure(onUnauthorized: () => void) {
  const [data, setData] = useState<InfrastructureResponse | null>(null);
  const [loadedAt, setLoadedAt] = useState<Date | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [reload, setReload] = useState(0);
  const unauthorized = useRef(onUnauthorized);
  unauthorized.current = onUnauthorized;
  const refresh = useCallback(() => setReload((value) => value + 1), []);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    let timer: ReturnType<typeof setTimeout> | undefined;
    api<InfrastructureResponse>("GET", "/api/infrastructure", undefined, { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) { setData(response); setLoadedAt(new Date()); setError(""); } })
      .catch((cause) => {
        if (controller.signal.aborted) return;
        if (cause instanceof ApiError && cause.status === 401) unauthorized.current(); else setError(errorText(cause));
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
          timer = setTimeout(refresh, REFRESH_MS);
        }
      });
    return () => { controller.abort(); clearTimeout(timer); };
  }, [refresh, reload]);

  return { data, loadedAt, error, loading, refresh };
}
