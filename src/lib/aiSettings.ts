import { invoke } from '@tauri-apps/api/core';
import type { ModelProfile, ModelSelection, Provider, ProviderModel } from '../types';

export interface PersistedAiSettings {
  version: 2;
  providers: Provider[];
  active: ModelSelection | null;
}

const EMPTY: PersistedAiSettings = { version: 2, providers: [], active: null };

export async function loadAiSettings(): Promise<PersistedAiSettings> {
  try {
    const raw = await invoke<string>('load_ai_settings');
    const parsed = JSON.parse(raw);

    if (Array.isArray(parsed?.providers)) {
      return {
        version: 2,
        providers: parsed.providers,
        active: parsed.active ?? null,
      };
    }

    return EMPTY;
  } catch (err) {
    console.error('Failed to load AI settings:', err);
    return EMPTY;
  }
}

export async function saveAiSettings(settings: PersistedAiSettings): Promise<void> {
  await invoke<void>('save_ai_settings', { json: JSON.stringify(settings) });
}

/*
 * ---------------------------------------------------------------------------
 * Naming helpers
 * ---------------------------------------------------------------------------
 */

const KNOWN_HOSTS: [RegExp, string][] = [
  [/api\.openai\.com/, 'OpenAI'],
  [/openrouter\.ai/, 'OpenRouter'],
  [/api\.groq\.com/, 'Groq'],
  [/api\.together\.(xyz|ai)/, 'Together'],
  [/api\.deepseek\.com/, 'DeepSeek'],
  [/api\.mistral\.ai/, 'Mistral'],
  [/api\.x\.ai/, 'xAI'],
  [/api\.anthropic\.com/, 'Anthropic'],
  [/generativelanguage\.googleapis\.com/, 'Google'],
  [/api\.fireworks\.ai/, 'Fireworks'],
  [/:11434\b/, 'Ollama'],
  [/:1234\b/, 'LM Studio'],
];

export function guessProviderName(baseUrl: string): string | undefined {
  const url = baseUrl.trim();
  for (const [re, name] of KNOWN_HOSTS) if (re.test(url)) return name;
  try {
    const host = new URL(url).hostname.replace(/^api\./, '');
    return host || undefined;
  } catch {
    return undefined;
  }
}

/** `owned_by` values that say nothing about who made the model. */
const GENERIC_OWNERS = new Set([
  '',
  'system',
  'user',
  'library',
  'organization',
  'organization-owner',
  'openai-internal',
  'llamacpp',
  'vllm',
]);

const DEVELOPER_BY_PREFIX: [RegExp, string][] = [
  [/^(gpt|o\d|chatgpt|text-embedding|dall-e|whisper|tts|omni-moderation|codex)/i, 'openai'],
  [/^claude/i, 'anthropic'],
  [/^(gemini|gemma)/i, 'google'],
  [/^(llama|meta-llama)/i, 'meta'],
  [/^(mistral|mixtral|codestral|ministral|pixtral|magistral|devstral)/i, 'mistral'],
  [/^qwen/i, 'qwen'],
  [/^deepseek/i, 'deepseek'],
  [/^grok/i, 'xai'],
  [/^phi/i, 'microsoft'],
  [/^(command|aya)/i, 'cohere'],
  [/^kimi/i, 'moonshotai'],
  [/^glm/i, 'z-ai'],
];

/**
 * Splits a model id into developer + name.
 *  - "meta-llama/llama-3.3-70b" → developer "meta-llama" (id already namespaced)
 *  - otherwise a meaningful `owned_by`, otherwise a guess from the name prefix
 *  - no developer when nothing fits, rather than a wrong one
 */
export function splitModelId(model: Pick<ProviderModel, 'id' | 'ownedBy'>): {
  developer?: string;
  name: string;
} {
  const slash = model.id.indexOf('/');
  if (slash > 0) {
    return { developer: model.id.slice(0, slash), name: model.id.slice(slash + 1) };
  }

  const owner = model.ownedBy?.trim().toLowerCase();
  if (owner && !GENERIC_OWNERS.has(owner)) return { developer: owner, name: model.id };

  for (const [re, dev] of DEVELOPER_BY_PREFIX) {
    if (re.test(model.id)) return { developer: dev, name: model.id };
  }

  return { name: model.id };
}

/**
 * Short provider label for compact UI: the main domain name of the base URL.
 *   https://openrouter.ai/api/v1                     → "openrouter"
 *   https://generativelanguage.googleapis.com/v1beta → "googleapis"
 *   https://llm.example.co.uk/v1                     → "example"
 *   http://localhost:11434/v1                        → "localhost:11434"
 * Falls back to the provider's name when the URL can't be parsed.
 */
export function providerDomainLabel(provider: Pick<Provider, 'name' | 'baseUrl'>): string {
  let url: URL;
  try {
    url = new URL(provider.baseUrl.trim());
  } catch {
    return provider.name;
  }

  const host = url.hostname.toLowerCase();
  const isIp = /^[\d.]+$/.test(host) || host.includes(':');
  const parts = host.split('.').filter(Boolean);

  // Local servers and IPs have no domain to shorten; the port tells them apart.
  if (isIp || parts.length < 2) {
    return url.port ? `${host}:${url.port}` : host || provider.name;
  }

  // Two-level public suffixes like co.uk / com.au: step one label further left.
  const tld = parts[parts.length - 1];
  const second = parts[parts.length - 2];
  const twoLevelSuffix = tld.length === 2 && second.length <= 3 && parts.length >= 3;

  return twoLevelSuffix ? parts[parts.length - 3] : second;
}

/**
 * Short labels for every provider, keyed by id. When two providers share a
 * domain (e.g. two OpenRouter accounts), those fall back to their names so
 * the picker can still tell them apart.
 */
export function providerShortLabels(providers: Provider[]): Map<string, string> {
  const domains = providers.map((p) => providerDomainLabel(p));
  const counts = new Map<string, number>();
  for (const d of domains) counts.set(d, (counts.get(d) ?? 0) + 1);

  return new Map(
    providers.map((p, i) => [p.id, counts.get(domains[i])! > 1 ? p.name : domains[i]])
  );
}

/** "openrouter/anthropic/claude-sonnet-4.5", or "localhost:11434/my-model" when the developer is unknown. */
export function formatModelLabel(providerLabel: string, model: ProviderModel): string {
  const { developer, name } = splitModelId(model);
  return [providerLabel, developer, name].filter(Boolean).join('/');
}

export function resolveProfile(
  providers: Provider[],
  selection: ModelSelection | null
): ModelProfile | null {
  if (!selection) return null;
  const provider = providers.find((p) => p.id === selection.providerId);
  const model = provider?.models.find((m) => m.id === selection.modelId);
  if (!provider || !model) return null;

  return {
    id: `${provider.id}::${model.id}`,
    label: formatModelLabel(providerShortLabels(providers).get(provider.id)!, model),
    model: model.id,
    baseUrl: provider.baseUrl,
    apiKey: provider.apiKey,
    stream: provider.stream,
    vision: !!model.vision,
  };
}

/** The saved selection if it still exists and is enabled, else the first enabled model. */
export function reconcileSelection(
  providers: Provider[],
  selection: ModelSelection | null
): ModelSelection | null {
  const stillValid =
    selection &&
    providers
      .find((p) => p.id === selection.providerId)
      ?.models.some((m) => m.id === selection.modelId && m.enabled);
  if (stillValid) return selection;

  for (const p of providers) {
    const m = p.models.find((x) => x.enabled);
    if (m) return { providerId: p.id, modelId: m.id };
  }
  return null;
}

export interface RemoteModel {
  id: string;
  owned_by?: string | null;
}

export function fetchModels(baseUrl: string, apiKey: string): Promise<RemoteModel[]> {
  return invoke<RemoteModel[]>('list_models', { baseUrl, apiKey });
}
