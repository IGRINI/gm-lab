import { useCallback, useEffect, useState } from "react";
import { normalizeModelBinding, resolveModelBinding } from "./connectorCatalog.js";

// Shared state machine for the three architect surfaces. A restored server
// binding locks the connector; a fresh history starts from the application
// default and locks as soon as its first request is submitted. The model never
// locks and can be changed between turns.
export default function useConnectorModelBinding(initialBinding, connectors, models) {
  const [modelBinding, setModelBinding] = useState(() => normalizeModelBinding(initialBinding));
  const [connectorLocked, setConnectorLocked] = useState(false);
  const [bindingLoading, setBindingLoading] = useState(false);
  const [bindingLoadFailed, setBindingLoadFailed] = useState(false);

  useEffect(() => {
    if (connectorLocked) return;
    setModelBinding((current) => resolveModelBinding(
      current.connector_id ? current : initialBinding,
      connectors,
      models
    ));
  }, [initialBinding, connectors, models, connectorLocked]);

  const resetModelBinding = useCallback(
    (persistedBinding) => {
      const persisted = normalizeModelBinding(persistedBinding);
      setModelBinding(resolveModelBinding(
        persisted.connector_id ? persisted : initialBinding,
        connectors,
        models
      ));
      setConnectorLocked(Boolean(persisted.connector_id));
      setBindingLoading(false);
      setBindingLoadFailed(false);
    },
    [initialBinding, connectors, models]
  );

  const lockConnector = useCallback(() => setConnectorLocked(true), []);

  return {
    modelBinding,
    setModelBinding,
    connectorLocked,
    bindingLoading,
    setBindingLoading,
    bindingLoadFailed,
    setBindingLoadFailed,
    lockConnector,
    resetModelBinding,
  };
}
