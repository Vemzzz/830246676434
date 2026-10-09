"use client";

import { useEffect, useMemo, useState } from "react";
import Card from "@/shared/components/Card";
import Badge from "@/shared/components/Badge";

// Human labels for the cadence enum; null renders as an explicit unknown ("—").
const REFRESH_LABELS = {
  daily: "Daily",
  weekly: "Weekly",
  monthly: "Monthly",
  "one-time": "One-time",
  rolling: "Rolling",
};

// Only cadences without an exact shared Badge variant need a colour override.
const REFRESH_BADGE = {
  daily: { variant: "success" },
  weekly: { variant: "info" },
  "one-time": { variant: "warning" },
  monthly: { variant: "default", className: "bg-purple-500/10 text-purple-600 dark:text-purple-400" },
  rolling: { variant: "default", className: "bg-cyan-500/10 text-cyan-600 dark:text-cyan-400" },
};

/**
 * Free-tier catalogue, grouped per provider.
 *
 * The flat 131-row table made the list unusable: rows repeated the provider name
 * on every line and nothing distinguished a provider you have an account for from
 * one you cannot use yet. Providers are grouped now, connected ones sort first,
 * and the rest stay behind a toggle (collapsed by default) with a search box.
 */
export default function FreeTierList({ tiers = [] }) {
  const [activeProviders, setActiveProviders] = useState(null); // null = unknown yet
  const [query, setQuery] = useState("");
  const [showUnconnected, setShowUnconnected] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetch("/api/providers", { cache: "no-store" })
      .then((res) => (res.ok ? res.json() : null))
      .then((data) => {
        if (cancelled || !data) return;
        const ids = new Set();
        for (const conn of data.connections || []) {
          if (conn.isActive === false) continue;
          if (conn.provider) ids.add(conn.provider);
          if (conn.alias) ids.add(conn.alias);
        }
        setActiveProviders(ids);
      })
      .catch(() => {
        if (!cancelled) setActiveProviders(new Set());
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const groups = useMemo(() => {
    const byProvider = new Map();
    for (const tier of tiers) {
      const id = tier.providerId;
      if (!byProvider.has(id)) {
        byProvider.set(id, {
          id,
          name: tier.providerName || id,
          alias: tier.providerAlias || null,
          refresh: tier.freeRefresh || null,
          models: [],
        });
      }
      const group = byProvider.get(id);
      if (!group.refresh && tier.freeRefresh) group.refresh = tier.freeRefresh;
      if (tier.modelId) {
        group.models.push({ id: tier.modelId, name: tier.modelName || tier.modelId });
      }
    }

    const needle = query.trim().toLowerCase();
    const all = [...byProvider.values()].map((group) => ({
      ...group,
      connected: Boolean(
        activeProviders &&
          (activeProviders.has(group.id) || (group.alias && activeProviders.has(group.alias))),
      ),
    }));

    const matches = needle
      ? all.filter(
          (g) =>
            g.name.toLowerCase().includes(needle) ||
            (g.alias || "").toLowerCase().includes(needle) ||
            g.models.some(
              (m) =>
                m.name.toLowerCase().includes(needle) || m.id.toLowerCase().includes(needle),
            ),
        )
      : all;

    return matches.sort((a, b) => {
      if (a.connected !== b.connected) return a.connected ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
  }, [tiers, activeProviders, query]);

  const connectedCount = groups.filter((g) => g.connected).length;
  const hiddenCount = activeProviders ? groups.length - connectedCount : 0;

  const visible = showUnconnected || query.trim() ? groups : groups.filter((g) => g.connected);

  return (
    <Card
      padding="lg"
      title="Available free tiers"
      subtitle="Provider free allowances from the registry. Connected providers first; the rest are behind the toggle."
    >
      {tiers.length === 0 ? (
        <p className="py-6 text-center text-sm text-text-muted">No free tiers found.</p>
      ) : (
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Filter by provider or model…"
              aria-label="Filter free tiers by provider or model"
              className="min-w-[12rem] flex-1 rounded border border-border bg-surface px-2.5 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-primary/50"
            />
            <span className="text-[11px] text-text-muted">
              {connectedCount} connected
              {activeProviders === null ? " (checking…)" : ` · ${hiddenCount} without an account`}
            </span>
            {hiddenCount > 0 && !query.trim() && (
              <button
                type="button"
                onClick={() => setShowUnconnected((v) => !v)}
                aria-pressed={showUnconnected}
                className="rounded border border-border px-2.5 py-1.5 text-xs text-text-muted transition-colors hover:border-primary hover:text-primary"
              >
                {showUnconnected ? "Hide" : "Show"} providers without an account
              </button>
            )}
          </div>

          {visible.length === 0 ? (
            <p className="py-6 text-center text-sm text-text-muted">
              {activeProviders === null
                ? "Checking your connections…"
                : "No free tier matches. Add a connection for a provider below, or clear the filter."}
            </p>
          ) : (
            <div className="flex flex-col divide-y divide-black/5 dark:divide-white/5">
              {visible.map((group) => {
                const badge = REFRESH_BADGE[group.refresh] || { variant: "default" };
                return (
                  <div key={group.id} className="flex flex-col gap-1.5 py-3">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-medium text-text-primary">{group.name}</span>
                      {group.alias && group.alias !== group.name && (
                        <span className="font-mono text-[10px] text-text-muted">{group.alias}</span>
                      )}
                      {group.connected ? (
                        <Badge size="sm" variant="success">
                          Connected
                        </Badge>
                      ) : activeProviders ? (
                        <span className="text-[11px] italic text-text-muted">no account yet</span>
                      ) : null}
                      {group.refresh ? (
                        <Badge size="sm" variant={badge.variant} className={badge.className}>
                          {REFRESH_LABELS[group.refresh] || group.refresh}
                        </Badge>
                      ) : (
                        <span
                          className="text-xs italic text-text-muted"
                          title="Reset cadence not documented"
                          aria-label="Reset cadence not documented"
                        >
                          —
                        </span>
                      )}
                      <span className="ml-auto text-[11px] text-text-muted">
                        {group.models.length === 0
                          ? "free tier available"
                          : `${group.models.length} free model${group.models.length > 1 ? "s" : ""}`}
                      </span>
                    </div>

                    {group.models.length > 0 && (
                      <div className="flex flex-wrap gap-1.5">
                        {group.models.map((model) => (
                          <span
                            key={model.id}
                            title={model.id}
                            className="rounded bg-black/[0.04] px-1.5 py-0.5 font-mono text-[11px] text-text-muted dark:bg-white/[0.06]"
                          >
                            {model.name}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </Card>
  );
}
