import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { PROVIDER_MODELS, PROVIDER_ID_TO_ALIAS } from "../../open-sse/config/providerModels.js";
import REGISTRY from "../../open-sse/providers/registry/index.js";
import { getCapabilitiesForModel } from "../../open-sse/providers/capabilities.js";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const backendDir = path.resolve(scriptDir, "..");
const providerDefinitions = Object.fromEntries(REGISTRY.map((provider) => [provider.id, provider]));
const providerOrder = Object.fromEntries(REGISTRY.map((provider, index) => [provider.id, index]));
const providerIdByAlias = Object.fromEntries(
  Object.entries(PROVIDER_ID_TO_ALIAS).map(([id, alias]) => [alias, id]),
);
const modelKind = (model) => {
  const kind = model?.kind || model?.type;
  return kind === "imageToText" ? "llm" : (kind || "llm");
};
const copyModelMetadata = (model) => ({
  id: model.id,
  name: model.name || model.id,
  kind: modelKind(model),
  ...(model.params !== undefined ? { params: model.params } : {}),
  ...(model.capabilities !== undefined ? { capabilities: model.capabilities } : {}),
  ...(model.options !== undefined ? { options: model.options } : {}),
  ...(model.dimensions !== undefined ? { dimensions: model.dimensions } : {}),
  ...(model.contextWindow !== undefined ? { contextWindow: model.contextWindow } : {}),
});

const providers = Object.entries(PROVIDER_MODELS).map(([alias, models]) => {
  const id = providerIdByAlias[alias] || alias;
  const definition = providerDefinitions[id] || {};
  return {
    id,
    alias,
    aliases: definition.aliases || [],
    category: definition.category || null,
    displayName: definition.display?.name || alias,
    headers: definition.transport?.headers || {},
    searchConfig: definition.searchConfig
      ? {
          searchTypes: definition.searchConfig.searchTypes,
          exaSearchTypes: definition.searchConfig.exaSearchTypes,
          maxMaxResults: definition.searchConfig.maxMaxResults,
          requiredOptions: definition.searchConfig.requiredOptions,
        }
      : null,
    hasSearchViaChat: Boolean(definition.searchViaChat),
    hasFetchConfig: Boolean(definition.fetchConfig),
    usageSupported: Boolean(definition.features?.usage),
    usageApikeySupported: Boolean(definition.features?.usageApikey),
    usageOrder: providerOrder[id] ?? 1_000_000_000,
    noAuth: definition.noAuth === true,
    authModes: definition.authModes
      || (definition.category === "apikey" ? ["apikey"] : []),
    serviceKinds: definition.serviceKinds || ["llm"],
    models: models
      .filter((model) => modelKind(model) === "llm")
      .map((model) => ({
        id: model.id,
        name: model.name || model.id,
        capabilities: getCapabilitiesForModel(id, model.id),
      })),
    allModels: models.map(copyModelMetadata),
  };
});

const includedProviderIds = new Set(providers.map((provider) => provider.id));
for (const definition of REGISTRY) {
  if (
    (!definition.searchConfig
      && !definition.fetchConfig
      && !definition.features?.usage
      && !definition.features?.usageApikey)
    || includedProviderIds.has(definition.id)
  ) {
    continue;
  }
  providers.push({
    id: definition.id,
    alias: definition.alias || definition.id,
    aliases: definition.aliases || [],
    category: definition.category || null,
    displayName: definition.display?.name || definition.alias || definition.id,
    headers: definition.transport?.headers || {},
    searchConfig: definition.searchConfig
      ? {
          searchTypes: definition.searchConfig.searchTypes,
          exaSearchTypes: definition.searchConfig.exaSearchTypes,
          maxMaxResults: definition.searchConfig.maxMaxResults,
          requiredOptions: definition.searchConfig.requiredOptions,
        }
      : null,
    hasSearchViaChat: Boolean(definition.searchViaChat),
    hasFetchConfig: Boolean(definition.fetchConfig),
    usageSupported: Boolean(definition.features?.usage),
    usageApikeySupported: Boolean(definition.features?.usageApikey),
    usageOrder: providerOrder[definition.id] ?? 1_000_000_000,
    noAuth: definition.noAuth === true,
    authModes: definition.authModes
      || (definition.category === "apikey" ? ["apikey"] : []),
    serviceKinds: definition.serviceKinds || [],
    models: [],
    allModels: [],
  });
  includedProviderIds.add(definition.id);
}

const outputPath = path.join(backendDir, "models.json");
fs.writeFileSync(outputPath, `${JSON.stringify({ providers }, null, 2)}\n`);
console.log(`Exported ${providers.length} providers to ${outputPath}`);
