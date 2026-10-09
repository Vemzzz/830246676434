"use client";

import { useEffect, useState } from "react";
import { Button, Card, Input } from "@/shared/components";

export default function GeminiSearchModelControl({ defaultModel }) {
  const [model, setModel] = useState("");
  const [modelOverrides, setModelOverrides] = useState({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetch("/api/settings", { cache: "no-store" })
      .then(async (response) => {
        const data = await response.json();
        if (!response.ok) throw new Error(data.error || "Could not load Gemini search settings");
        return data;
      })
      .then((settings) => {
        if (cancelled) return;
        const overrides = settings.webSearchModelOverrides && typeof settings.webSearchModelOverrides === "object"
          && !Array.isArray(settings.webSearchModelOverrides)
          ? settings.webSearchModelOverrides
          : {};
        setModelOverrides(overrides);
        setModel(typeof overrides.gemini === "string" ? overrides.gemini : "");
        setLoading(false);
      })
      .catch((loadError) => {
        if (cancelled) return;
        setError(loadError.message);
        setLoading(false);
      });
    return () => { cancelled = true; };
  }, []);

  const handleSave = async () => {
    const normalizedModel = model.trim();
    const nextOverrides = { ...modelOverrides };
    if (normalizedModel) nextOverrides.gemini = normalizedModel;
    else delete nextOverrides.gemini;

    setSaving(true);
    setError("");
    setSaved(false);
    try {
      const response = await fetch("/api/settings", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ webSearchModelOverrides: nextOverrides }),
      });
      const data = await response.json();
      if (!response.ok) throw new Error(data.error || "Could not save Gemini search model");
      setModelOverrides(nextOverrides);
      setModel(normalizedModel);
      setSaved(true);
    } catch (saveError) {
      setError(saveError.message);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card padding="xs" className="mt-2">
      <div className="flex flex-col gap-3">
        <Input
          label="Gemini Web Search Model"
          placeholder={defaultModel}
          value={model}
          onChange={(event) => {
            setModel(event.target.value);
            setSaved(false);
          }}
          disabled={loading || saving}
          hint={`Leave empty to use the default: ${defaultModel}`}
        />
        <div className="flex items-center justify-between gap-2">
          {loading ? (
            <p className="text-xs text-text-muted">Loading saved model…</p>
          ) : error ? (
            <p className="text-xs text-red-500">{error}</p>
          ) : (
            <p className="text-xs text-green-600">{saved ? "Model setting saved." : ""}</p>
          )}
          <Button
            size="sm"
            loading={saving}
            disabled={loading || !!error}
            onClick={handleSave}
          >
            Save Model
          </Button>
        </div>
      </div>
    </Card>
  );
}
