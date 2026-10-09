"use client";

import { useCallback, useEffect, useState } from "react";
import { Button, ModelSelectModal } from "@/shared/components";
import ProviderIcon from "@/shared/components/ProviderIcon";
import { deriveProfileNameFromModel } from "./codexConfig";

export default function CodexProfilesSection({ activeProviders, modelAliases }) {
  const [profiles, setProfiles] = useState([]);
  const [aliasInput, setAliasInput] = useState("");
  const [modelInput, setModelInput] = useState("");
  const [profileModalOpen, setProfileModalOpen] = useState(false);
  const [creatingProfile, setCreatingProfile] = useState(false);
  const [deletingProfile, setDeletingProfile] = useState(null);
  const [copiedCommand, setCopiedCommand] = useState("");
  const [error, setError] = useState("");

  const fetchProfiles = useCallback(async () => {
    try {
      const res = await fetch("/api/cli-tools/codex-profiles", { cache: "no-store" });
      const data = await res.json();
      if (res.ok) setProfiles(data.profiles || []);
    } catch (err) {
      console.log("Error fetching codex profiles:", err);
    }
  }, []);

  useEffect(() => {
    // Profiles describe files in ~/.codex, so they must be fetched on mount.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- mount-time fetch of external state
    fetchProfiles();
  }, [fetchProfiles]);

  const applyAliasFor = (modelOrId) => {
    const modelId = typeof modelOrId === "string" ? modelOrId : modelOrId?.value || modelOrId?.id;
    if (!modelId) return;
    setModelInput(modelId);
    const providerName = typeof modelOrId === "object" && modelOrId?.provider
      ? modelOrId.provider
      : modelId.includes("/") ? modelId.split("/")[0] : modelId;
    setAliasInput(deriveProfileNameFromModel(providerName, profiles.map((p) => p.name)));
  };

  const handleAddProfile = async () => {
    const cleanAlias = aliasInput.trim().toLowerCase().replace(/[^a-z0-9_-]/g, "");
    const cleanModel = modelInput.trim();
    if (!cleanAlias || !cleanModel) return;

    setCreatingProfile(true);
    setError("");
    try {
      const res = await fetch("/api/cli-tools/codex-profiles", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: cleanAlias, model: cleanModel }),
      });
      const data = await res.json();
      if (res.ok) {
        setAliasInput("");
        setModelInput("");
        fetchProfiles();
      } else {
        setError(data.error || "Failed to add model");
      }
    } catch (err) {
      setError(err.message);
    } finally {
      setCreatingProfile(false);
    }
  };

  const handleDeleteProfile = async (name) => {
    setDeletingProfile(name);
    try {
      const res = await fetch("/api/cli-tools/codex-profiles", {
        method: "DELETE",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name }),
      });
      if (res.ok) fetchProfiles();
    } catch (err) {
      console.log("Error deleting codex profile:", err);
    } finally {
      setDeletingProfile(null);
    }
  };

  const handleCopyCommand = async (cmd) => {
    try {
      await navigator.clipboard.writeText(cmd);
      setCopiedCommand(cmd);
      setTimeout(() => setCopiedCommand(""), 2000);
    } catch (err) {
      console.log("Copy failed", err);
    }
  };

  return (
    <div className="mt-4 pt-4 border-t border-border flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <span className="text-xs font-semibold text-text-main flex items-center gap-1.5">
          <span className="material-symbols-outlined text-[16px] text-primary">layers</span>
          Additional Models
        </span>
        {profiles.length > 0 && (
          <span className="px-1.5 py-0.5 bg-primary/10 text-primary text-[10px] font-medium rounded-full">
            {profiles.length}
          </span>
        )}
      </div>

      <div className="flex flex-col gap-1.5 p-2.5 bg-surface/40 border border-border rounded-lg">
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-[10rem_1fr_auto_auto] sm:items-center">
          <input
            type="text"
            value={aliasInput}
            onChange={(e) => setAliasInput(e.target.value.toLowerCase().replace(/[^a-z0-9_-]/g, ""))}
            placeholder="Alias (e.g. claude)"
            className="w-full min-w-0 px-2.5 py-1.5 bg-surface rounded border border-border text-xs font-mono focus:outline-none focus:ring-1 focus:ring-primary/50"
            onKeyDown={(e) => e.key === "Enter" && handleAddProfile()}
          />
          <div className="relative w-full min-w-0">
            <input
              type="text"
              value={modelInput}
              onChange={(e) => applyAliasFor(e.target.value)}
              placeholder="provider/model-id"
              className="w-full min-w-0 pl-2.5 pr-7 py-1.5 bg-surface rounded border border-border text-xs focus:outline-none focus:ring-1 focus:ring-primary/50"
              onKeyDown={(e) => e.key === "Enter" && handleAddProfile()}
            />
            {modelInput && (
              <button
                onClick={() => { setModelInput(""); setAliasInput(""); }}
                className="absolute right-1 top-1/2 -translate-y-1/2 p-0.5 text-text-muted hover:text-red-500 rounded transition-colors"
                title="Clear"
              >
                <span className="material-symbols-outlined text-[14px]">close</span>
              </button>
            )}
          </div>
          <button
            onClick={() => setProfileModalOpen(true)}
            disabled={!activeProviders?.length}
            className={`w-full sm:w-auto rounded border px-2.5 py-1.5 text-xs transition-colors whitespace-nowrap sm:shrink-0 ${
              activeProviders?.length
                ? "bg-surface border-border text-text-main hover:border-primary cursor-pointer"
                : "opacity-50 cursor-not-allowed border-border"
            }`}
          >
            Select Model
          </button>
          <Button
            variant="primary"
            size="sm"
            onClick={handleAddProfile}
            disabled={!aliasInput.trim() || !modelInput.trim() || creatingProfile}
            loading={creatingProfile}
            className="!h-7.5 whitespace-nowrap"
          >
            <span className="material-symbols-outlined text-[15px] mr-1">add</span>
            Add
          </Button>
        </div>
        {!!error && <p className="text-[11px] text-red-500 break-words">{error}</p>}
      </div>

      {profiles.length === 0 ? (
        <div className="flex flex-col items-center justify-center p-4 border border-dashed border-border rounded-lg text-center bg-surface/30">
          <span className="material-symbols-outlined text-[20px] text-text-muted mb-1 opacity-60">terminal</span>
          <p className="text-xs text-text-muted">
            Only the main model is active. Add an alias above to configure more models for Codex CLI.
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
          {profiles.map((p) => {
            const providerId = p.model.includes("/") ? p.model.split("/")[0] : p.name;
            const isCopied = copiedCommand === p.command;
            return (
              <div
                key={p.name}
                className="flex items-center justify-between gap-2 p-2.5 bg-surface/50 border border-border hover:border-border-hover rounded-lg transition-colors group"
              >
                <div className="flex items-center gap-2.5 min-w-0">
                  <div className="size-6 flex items-center justify-center shrink-0 rounded bg-black/5 dark:bg-white/5 p-0.5">
                    <ProviderIcon providerId={providerId} size={18} fallbackText={p.name.slice(0, 2).toUpperCase()} />
                  </div>
                  <div className="min-w-0 flex flex-col">
                    <span className="font-medium text-xs text-text-main truncate">{p.name}</span>
                    <span className="text-[11px] text-text-muted truncate font-mono">{p.model}</span>
                  </div>
                </div>

                <div className="flex items-center gap-1 shrink-0">
                  <button
                    onClick={() => handleCopyCommand(p.command)}
                    className={`flex items-center gap-1 px-2 py-1 rounded text-[11px] font-mono border transition-all ${
                      isCopied
                        ? "bg-green-500/10 border-green-500/30 text-green-600 dark:text-green-400"
                        : "bg-surface border-border text-text-muted hover:text-text-main hover:border-primary/50 cursor-pointer"
                    }`}
                    title="Click to copy command"
                  >
                    <span className="material-symbols-outlined text-[13px]">{isCopied ? "check" : "terminal"}</span>
                    <span className="hidden md:inline">{p.command}</span>
                    <span className="md:hidden">copy</span>
                  </button>

                  <button
                    onClick={() => handleDeleteProfile(p.name)}
                    disabled={deletingProfile === p.name}
                    className="p-1 text-text-muted hover:text-red-500 opacity-0 group-hover:opacity-100 transition-opacity rounded"
                    title="Delete model"
                  >
                    <span className="material-symbols-outlined text-[15px]">close</span>
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {profileModalOpen && (
        <ModelSelectModal
          isOpen={profileModalOpen}
          onClose={() => setProfileModalOpen(false)}
          onSelect={(model) => {
            setProfileModalOpen(false);
            applyAliasFor(model);
          }}
          selectedModel={modelInput}
          activeProviders={activeProviders}
          modelAliases={modelAliases}
          title="Select Model for Codex CLI"
        />
      )}
    </div>
  );
}
