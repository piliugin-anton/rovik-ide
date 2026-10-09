// client.ts
//
// A Language Server Protocol client for Monaco, for servers the backend
// runs (src-tauri/src/lsp/). Deliberately small: document sync,
// diagnostics, hover, completion, signature help, go to definition / type
// definition / implementation, references, highlights and formatting.
//
// Monaco 0.57 ships an LSP client, but it doesn't fit: it starts servers
// without a workspace root (rust-analyzer then understands nothing across
// files), lowercases file URIs (wrong on case-sensitive file systems),
// fails when a definition is in a file that isn't open, sends every open
// file to every server, and registers each server for every language.
import * as monaco from 'monaco-editor';
import { Channel, invoke } from '@tauri-apps/api/core';
import { readFileContent } from '../commands';
import { languageForFile } from '../language';
import { modelUriFor } from '../intellisense';
import { scheduleModelPrune } from '../models';

/* eslint-disable @typescript-eslint/no-explicit-any */
type Json = any;

type LspEvent =
  | { type: 'message'; data: string }
  | { type: 'log'; line: string }
  | { type: 'exit'; code: number | null };

export interface ServerStatus {
  state: 'starting' | 'indexing' | 'ready' | 'warning' | 'error' | 'stopped';
  message?: string;
  /** 0..1 while indexing, when the server says how far along it is. */
  progress?: number;
}

export interface LspClientOptions {
  /** For messages and markers, e.g. "rust-analyzer". */
  name: string;
  command: string;
  args?: string[];
  root: string;
  /** The Monaco language this server is for ("rust"). */
  languageId: string;
  /** File extensions it handles (".rs"). */
  extensions: string[];
  initializationOptions?: Json;
  /** Server-specific client capabilities, merged into `experimental`. */
  experimental?: Json;
  onStatus?: (status: ServerStatus) => void;
  /** The server asked to tell the user something (errors and warnings). */
  onShowMessage?: (type: 'error' | 'warning' | 'info', message: string) => void;
  /** The process ended; `expected` when stop() ended it. */
  onExit?: (code: number | null, expected: boolean) => void;
}

const CHANGE_DEBOUNCE_MS = 150;
/** Files a definition or reference result opens as models, at most. */
const MAX_RESULT_FILES = 60;

/** LSP CompletionItemKind (1-based) to Monaco's. */
const COMPLETION_KIND = [
  undefined,
  'Text',
  'Method',
  'Function',
  'Constructor',
  'Field',
  'Variable',
  'Class',
  'Interface',
  'Module',
  'Property',
  'Unit',
  'Value',
  'Enum',
  'Keyword',
  'Snippet',
  'Color',
  'File',
  'Reference',
  'Folder',
  'EnumMember',
  'Constant',
  'Struct',
  'Event',
  'Operator',
  'TypeParameter',
] as const;

export class LspClient {
  private serverId: string | null = null;
  private nextRequestId = 1;
  private pending = new Map<number, { resolve: (v: Json) => void; reject: (e: Error) => void }>();
  private capabilities: Json = {};
  private providers: monaco.IDisposable[] = [];
  private docs = new Map<
    string,
    { model: monaco.editor.ITextModel; version: number; timer?: number; subs: monaco.IDisposable[] }
  >();
  private diagnostics = new Map<string, Json[]>();
  private progress = new Map<
    string | number,
    { title?: string; message?: string; percentage?: number }
  >();
  private quiescent = false;
  private health: 'ok' | 'warning' | 'error' = 'ok';
  private healthMessage?: string;
  private stopping = false;
  private exited = false;
  private rawCompletion = new WeakMap<monaco.languages.CompletionItem, Json>();

  constructor(private readonly opts: LspClientOptions) {}

  get running(): boolean {
    return this.serverId !== null && !this.exited;
  }

  /** Starts the server, initializes it and starts serving the editor. */
  async start(): Promise<void> {
    this.status({ state: 'starting' });
    const channel = new Channel<LspEvent>();
    channel.onmessage = (event) => {
      if (event.type === 'message') this.receive(event.data);
      else if (event.type === 'exit') this.handleExit(event.code);
    };
    this.serverId = await invoke<string>('lsp_start', {
      command: this.opts.command,
      args: this.opts.args ?? [],
      cwd: this.opts.root,
      onEvent: channel,
    });

    const rootUri = modelUriFor(this.opts.root).toString();
    const name = this.opts.root.split(/[\\/]/).filter(Boolean).pop() ?? this.opts.root;
    const result = await this.request('initialize', {
      processId: null,
      clientInfo: { name: 'Rovik IDE' },
      rootUri,
      rootPath: this.opts.root,
      workspaceFolders: [{ uri: rootUri, name }],
      initializationOptions: this.opts.initializationOptions,
      capabilities: {
        workspace: { workspaceFolders: true, configuration: false, applyEdit: false },
        window: { workDoneProgress: true, showMessage: {} },
        textDocument: {
          synchronization: { dynamicRegistration: false, didSave: true, willSave: false },
          publishDiagnostics: { relatedInformation: false, tagSupport: { valueSet: [1, 2] } },
          hover: { contentFormat: ['markdown', 'plaintext'] },
          completion: {
            contextSupport: true,
            completionItem: {
              snippetSupport: true,
              documentationFormat: ['markdown', 'plaintext'],
              deprecatedSupport: true,
              insertReplaceSupport: true,
              labelDetailsSupport: true,
              resolveSupport: { properties: ['documentation', 'detail'] },
            },
          },
          signatureHelp: {
            signatureInformation: {
              documentationFormat: ['markdown', 'plaintext'],
              parameterInformation: { labelOffsetSupport: true },
              activeParameterSupport: true,
            },
          },
          definition: { linkSupport: true },
          typeDefinition: { linkSupport: true },
          implementation: { linkSupport: true },
          references: {},
          documentHighlight: {},
          formatting: {},
        },
        experimental: this.opts.experimental,
      },
    });
    this.capabilities = result?.capabilities ?? {};
    this.notify('initialized', {});
    this.registerProviders();
    this.syncModels();
    this.updateStatus();
  }

  /** Shuts the server down and stops serving the editor. */
  async stop(): Promise<void> {
    if (!this.serverId || this.stopping) return;
    this.stopping = true;
    const id = this.serverId;
    try {
      if (!this.exited) {
        await Promise.race([this.request('shutdown', null), sleep(2000)]);
        this.notify('exit', null);
      }
    } catch {
      // already gone
    }
    await invoke('lsp_stop', { id }).catch(() => {});
    this.serverId = null;
    this.dispose();
  }

  /** The user saved a file (rust-analyzer runs `cargo check` on save). */
  didSave(path: string) {
    const uri = modelUriFor(path).toString();
    if (!this.docs.has(uri)) return;
    this.flush(uri);
    this.notify('textDocument/didSave', { textDocument: { uri } });
  }

  /** A request to the server; null when it fails or was cancelled. */
  async request(method: string, params: Json, token?: monaco.CancellationToken): Promise<Json> {
    if (!this.serverId || this.exited) throw new Error(`${this.opts.name} isn't running`);
    const id = this.nextRequestId++;
    const result = new Promise<Json>((resolve, reject) =>
      this.pending.set(id, { resolve, reject })
    );
    const cancel = token?.onCancellationRequested(() => this.notify('$/cancelRequest', { id }));
    this.send({ jsonrpc: '2.0', id, method, params });
    try {
      return await result;
    } finally {
      cancel?.dispose();
    }
  }

  notify(method: string, params: Json) {
    if (!this.serverId || this.exited) return;
    this.send({ jsonrpc: '2.0', method, params });
  }

  private send(message: Json) {
    void invoke('lsp_send', { id: this.serverId, message: JSON.stringify(message) }).catch(() => {});
  }

  /* -------------------------------------------------------------------------
   * Incoming
   * ----------------------------------------------------------------------- */

  private receive(data: string) {
    let msg: Json;
    try {
      msg = JSON.parse(data);
    } catch {
      return;
    }
    if (msg.id !== undefined && msg.method === undefined) {
      const waiting = this.pending.get(msg.id);
      this.pending.delete(msg.id);
      if (!waiting) return;
      if (msg.error) waiting.reject(new Error(msg.error.message ?? 'request failed'));
      else waiting.resolve(msg.result ?? null);
    } else if (msg.id !== undefined) {
      this.respond(msg.id, msg.method, msg.params);
    } else if (msg.method) {
      this.onNotification(msg.method, msg.params);
    }
  }

  /** Requests from the server. Only what a client must answer. */
  private respond(id: number | string, method: string, params: Json) {
    let result: Json = null;
    switch (method) {
      case 'window/workDoneProgress/create':
      case 'client/registerCapability':
      case 'client/unregisterCapability':
        break;
      case 'workspace/configuration':
        result = (params?.items ?? []).map(() => null);
        break;
      case 'workspace/applyEdit':
        result = { applied: false };
        break;
      default:
        this.send({
          jsonrpc: '2.0',
          id,
          error: { code: -32601, message: `unsupported: ${method}` },
        });
        return;
    }
    this.send({ jsonrpc: '2.0', id, result });
  }

  private onNotification(method: string, params: Json) {
    switch (method) {
      case 'textDocument/publishDiagnostics':
        this.diagnostics.set(params.uri, params.diagnostics ?? []);
        this.applyDiagnostics(params.uri);
        break;
      case '$/progress': {
        const value = params?.value ?? {};
        if (value.kind === 'end') this.progress.delete(params.token);
        else {
          const prev = this.progress.get(params.token) ?? {};
          this.progress.set(params.token, {
            title: value.title ?? prev.title,
            message: value.message ?? prev.message,
            percentage: value.percentage ?? prev.percentage,
          });
        }
        this.updateStatus();
        break;
      }
      case 'experimental/serverStatus':
        this.quiescent = !!params?.quiescent;
        this.health = params?.health ?? 'ok';
        this.healthMessage = params?.message;
        this.updateStatus();
        break;
      case 'window/showMessage': {
        const type = params?.type === 1 ? 'error' : params?.type === 2 ? 'warning' : 'info';
        this.opts.onShowMessage?.(type, String(params?.message ?? ''));
        break;
      }
    }
  }

  private handleExit(code: number | null) {
    this.exited = true;
    for (const { reject } of this.pending.values()) reject(new Error(`${this.opts.name} exited`));
    this.pending.clear();
    const expected = this.stopping;
    this.dispose();
    this.status({ state: 'stopped' });
    this.opts.onExit?.(code, expected);
  }

  private dispose() {
    for (const p of this.providers) p.dispose();
    this.providers = [];
    for (const doc of this.docs.values()) {
      window.clearTimeout(doc.timer);
      for (const s of doc.subs) s.dispose();
      if (!doc.model.isDisposed()) monaco.editor.setModelMarkers(doc.model, this.opts.name, []);
    }
    this.docs.clear();
  }

  /* -------------------------------------------------------------------------
   * Status
   * ----------------------------------------------------------------------- */

  private status(s: ServerStatus) {
    this.opts.onStatus?.(s);
  }

  private updateStatus() {
    const active = [...this.progress.values()];
    if (active.length > 0) {
      const p = active[0];
      this.status({
        state: 'indexing',
        message: [p.title, p.message].filter(Boolean).join(': ') || undefined,
        progress: p.percentage !== undefined ? p.percentage / 100 : undefined,
      });
    } else if (this.health === 'error' || this.health === 'warning') {
      this.status({ state: this.health, message: this.healthMessage });
    } else {
      this.status({
        state: this.quiescent || !this.capabilities.experimental ? 'ready' : 'indexing',
      });
    }
  }

  /* -------------------------------------------------------------------------
   * Documents
   * ----------------------------------------------------------------------- */

  private isOurs(model: monaco.editor.ITextModel) {
    return (
      model.uri.scheme === 'file' &&
      this.opts.extensions.some((ext) => model.uri.path.endsWith(ext))
    );
  }

  /** Opens every matching model with the server, now and as they're created. */
  private syncModels() {
    for (const model of monaco.editor.getModels()) this.open(model);
    this.providers.push(monaco.editor.onDidCreateModel((model) => this.open(model)));
  }

  private open(model: monaco.editor.ITextModel) {
    if (!this.isOurs(model)) return;
    const uri = model.uri.toString();
    if (this.docs.has(uri)) return;
    const doc = {
      model,
      version: 1,
      timer: undefined as number | undefined,
      subs: [] as monaco.IDisposable[],
    };
    this.docs.set(uri, doc);
    this.notify('textDocument/didOpen', {
      textDocument: {
        uri,
        languageId: this.opts.languageId,
        version: doc.version,
        text: model.getValue(),
      },
    });
    // Whole-document sync, debounced: simple and exact.
    doc.subs.push(
      model.onDidChangeContent(() => {
        window.clearTimeout(doc.timer);
        doc.timer = window.setTimeout(() => this.flush(uri), CHANGE_DEBOUNCE_MS);
      }),
      model.onWillDispose(() => {
        window.clearTimeout(doc.timer);
        for (const s of doc.subs) s.dispose();
        this.docs.delete(uri);
        this.notify('textDocument/didClose', { textDocument: { uri } });
      })
    );
    this.applyDiagnostics(uri);
  }

  /** Sends a pending change now (before a request about that document). */
  private flush(uri: string) {
    const doc = this.docs.get(uri);
    if (!doc || doc.timer === undefined) return;
    window.clearTimeout(doc.timer);
    doc.timer = undefined;
    doc.version++;
    this.notify('textDocument/didChange', {
      textDocument: { uri, version: doc.version },
      contentChanges: [{ text: doc.model.getValue() }],
    });
  }

  private applyDiagnostics(uri: string) {
    const model = monaco.editor.getModel(monaco.Uri.parse(uri));
    if (!model || !this.isOurs(model)) return;
    const severity = [
      monaco.MarkerSeverity.Info,
      monaco.MarkerSeverity.Error,
      monaco.MarkerSeverity.Warning,
      monaco.MarkerSeverity.Info,
      monaco.MarkerSeverity.Hint,
    ];
    const markers = (this.diagnostics.get(uri) ?? []).map((d: Json): monaco.editor.IMarkerData => ({
      ...toRange(d.range),
      severity: severity[d.severity ?? 1] ?? monaco.MarkerSeverity.Info,
      message: d.message,
      source: d.source,
      code:
        typeof d.code === 'object'
          ? String(d.code?.value ?? '')
          : d.code !== undefined
            ? String(d.code)
            : undefined,
      tags: (d.tags ?? [])
        .map((t: number) =>
          t === 1 ? monaco.MarkerTag.Unnecessary : t === 2 ? monaco.MarkerTag.Deprecated : null
        )
        .filter((t: monaco.MarkerTag | null): t is monaco.MarkerTag => t !== null),
    }));
    monaco.editor.setModelMarkers(model, this.opts.name, markers);
  }

  /** A model for a result's file, opened from disk if it isn't one yet. */
  private async modelFor(uri: string): Promise<monaco.editor.ITextModel | null> {
    const parsed = monaco.Uri.parse(uri);
    const existing = monaco.editor.getModel(parsed);
    if (existing) return existing;
    if (parsed.scheme !== 'file') return null;
    try {
      const text = await readFileContent(parsed.fsPath);
      const model =
        monaco.editor.getModel(parsed) ??
        monaco.editor.createModel(text, languageForFile(parsed.path), parsed);
      // A model for a file no tab holds: counts toward the idle limit.
      scheduleModelPrune();
      return model;
    } catch {
      return null;
    }
  }

  /* -------------------------------------------------------------------------
   * Editor features
   * ----------------------------------------------------------------------- */

  private position(model: monaco.editor.ITextModel, position: monaco.Position) {
    const uri = model.uri.toString();
    this.flush(uri);
    return {
      textDocument: { uri },
      position: { line: position.lineNumber - 1, character: position.column - 1 },
    };
  }

  /** Requests about a document return null rather than throw: a server
   *  that's busy or restarting shouldn't break the editor. */
  private async ask(method: string, params: Json, token?: monaco.CancellationToken): Promise<Json> {
    try {
      return await this.request(method, params, token);
    } catch {
      return null;
    }
  }

  private async locations(result: Json): Promise<monaco.languages.Location[]> {
    const list: Json[] = !result ? [] : Array.isArray(result) ? result : [result];
    const files = [...new Set(list.map((l) => l.targetUri ?? l.uri))].slice(0, MAX_RESULT_FILES);
    // Definitions in files that aren't open need a model, or Monaco can't
    // show or open them (the editor's opener then opens a tab).
    await Promise.all(files.map((uri) => this.modelFor(uri)));
    return list
      .filter((l) => files.includes(l.targetUri ?? l.uri))
      .map((l) => ({
        uri: monaco.Uri.parse(l.targetUri ?? l.uri),
        range: toRange(l.targetSelectionRange ?? l.targetRange ?? l.range),
      }));
  }

  private registerProviders() {
    const selector: monaco.languages.LanguageSelector = {
      language: this.opts.languageId,
      scheme: 'file',
    };
    const caps = this.capabilities;
    const L = monaco.languages;
    const add = (d: monaco.IDisposable) => this.providers.push(d);

    if (caps.hoverProvider) {
      add(
        L.registerHoverProvider(selector, {
          provideHover: async (model, position, token) => {
            const r = await this.ask('textDocument/hover', this.position(model, position), token);
            if (!r?.contents) return null;
            const contents = (Array.isArray(r.contents) ? r.contents : [r.contents]).map(
              toMarkdown
            );
            return { contents, range: r.range ? toRange(r.range) : undefined };
          },
        })
      );
    }

    const locationProvider =
      (method: string) =>
      async (
        model: monaco.editor.ITextModel,
        position: monaco.Position,
        token: monaco.CancellationToken
      ) =>
        this.locations(await this.ask(method, this.position(model, position), token));

    if (caps.definitionProvider) {
      add(
        L.registerDefinitionProvider(selector, {
          provideDefinition: locationProvider('textDocument/definition'),
        })
      );
    }
    if (caps.typeDefinitionProvider) {
      add(
        L.registerTypeDefinitionProvider(selector, {
          provideTypeDefinition: locationProvider('textDocument/typeDefinition'),
        })
      );
    }
    if (caps.implementationProvider) {
      add(
        L.registerImplementationProvider(selector, {
          provideImplementation: locationProvider('textDocument/implementation'),
        })
      );
    }
    if (caps.referencesProvider) {
      add(
        L.registerReferenceProvider(selector, {
          provideReferences: async (model, position, _context, token) =>
            this.locations(
              await this.ask(
                'textDocument/references',
                { ...this.position(model, position), context: { includeDeclaration: true } },
                token
              )
            ),
        })
      );
    }
    if (caps.documentHighlightProvider) {
      add(
        L.registerDocumentHighlightProvider(selector, {
          provideDocumentHighlights: async (model, position, token) => {
            const r = await this.ask(
              'textDocument/documentHighlight',
              this.position(model, position),
              token
            );
            return (r ?? []).map((h: Json) => ({
              range: toRange(h.range),
              kind: (h.kind ?? 1) - 1,
            }));
          },
        })
      );
    }
    if (caps.documentFormattingProvider) {
      add(
        L.registerDocumentFormattingEditProvider(selector, {
          provideDocumentFormattingEdits: async (model, options, token) => {
            this.flush(model.uri.toString());
            const r = await this.ask(
              'textDocument/formatting',
              {
                textDocument: { uri: model.uri.toString() },
                options: { tabSize: options.tabSize, insertSpaces: options.insertSpaces },
              },
              token
            );
            return (r ?? []).map((e: Json) => ({ range: toRange(e.range), text: e.newText }));
          },
        })
      );
    }
    if (caps.signatureHelpProvider) {
      add(
        L.registerSignatureHelpProvider(selector, {
          signatureHelpTriggerCharacters: caps.signatureHelpProvider.triggerCharacters,
          signatureHelpRetriggerCharacters: caps.signatureHelpProvider.retriggerCharacters,
          provideSignatureHelp: async (model, position, token) => {
            const r = await this.ask(
              'textDocument/signatureHelp',
              this.position(model, position),
              token
            );
            if (!r?.signatures?.length) return null;
            return {
              value: {
                activeSignature: r.activeSignature ?? 0,
                activeParameter: r.activeParameter ?? 0,
                signatures: r.signatures.map((s: Json) => ({
                  label: s.label,
                  documentation: s.documentation ? toMarkdown(s.documentation) : undefined,
                  activeParameter: s.activeParameter,
                  parameters: (s.parameters ?? []).map((p: Json) => ({
                    label: p.label,
                    documentation: p.documentation ? toMarkdown(p.documentation) : undefined,
                  })),
                })),
              },
              dispose() {},
            };
          },
        })
      );
    }
    if (caps.completionProvider) {
      const canResolve = !!caps.completionProvider.resolveProvider;
      add(
        L.registerCompletionItemProvider(selector, {
          triggerCharacters: caps.completionProvider.triggerCharacters,
          provideCompletionItems: async (model, position, context, token) => {
            const r = await this.ask(
              'textDocument/completion',
              {
                ...this.position(model, position),
                context: {
                  triggerKind: context.triggerKind + 1,
                  triggerCharacter: context.triggerCharacter,
                },
              },
              token
            );
            if (!r) return { suggestions: [] };
            const items: Json[] = Array.isArray(r) ? r : (r.items ?? []);
            const word = model.getWordUntilPosition(position);
            const fallback = new monaco.Range(
              position.lineNumber,
              word.startColumn,
              position.lineNumber,
              word.endColumn
            );
            const suggestions = items.map((item) => {
              const edit = item.textEdit;
              const suggestion: monaco.languages.CompletionItem = {
                label: item.labelDetails
                  ? {
                      label: item.label,
                      detail: item.labelDetails.detail,
                      description: item.labelDetails.description,
                    }
                  : item.label,
                kind: L.CompletionItemKind[COMPLETION_KIND[item.kind] ?? 'Text'],
                detail: item.detail,
                documentation: item.documentation ? toMarkdown(item.documentation) : undefined,
                sortText: item.sortText,
                filterText: item.filterText,
                preselect: item.preselect,
                tags:
                  item.deprecated || item.tags?.includes(1)
                    ? [L.CompletionItemTag.Deprecated]
                    : undefined,
                insertText: edit?.newText ?? item.insertText ?? item.label,
                insertTextRules:
                  item.insertTextFormat === 2
                    ? L.CompletionItemInsertTextRule.InsertAsSnippet
                    : undefined,
                range: edit
                  ? edit.range
                    ? toRange(edit.range)
                    : { insert: toRange(edit.insert), replace: toRange(edit.replace) }
                  : fallback,
                // Auto-imports: `use` lines added along with the completion.
                additionalTextEdits: (item.additionalTextEdits ?? []).map((e: Json) => ({
                  range: toRange(e.range),
                  text: e.newText,
                })),
                commitCharacters: item.commitCharacters,
                // e.g. show a function's parameters after completing its name
                command: item.command?.command?.startsWith('editor.')
                  ? { id: item.command.command, title: item.command.title ?? '' }
                  : undefined,
              };
              if (canResolve) this.rawCompletion.set(suggestion, item);
              return suggestion;
            });
            return { suggestions, incomplete: !Array.isArray(r) && !!r.isIncomplete };
          },
          resolveCompletionItem: canResolve
            ? async (suggestion, token) => {
                const raw = this.rawCompletion.get(suggestion);
                if (!raw) return suggestion;
                const r = await this.ask('completionItem/resolve', raw, token);
                if (!r) return suggestion;
                return {
                  ...suggestion,
                  detail: r.detail ?? suggestion.detail,
                  documentation: r.documentation
                    ? toMarkdown(r.documentation)
                    : suggestion.documentation,
                };
              }
            : undefined,
        })
      );
    }
  }
}

/** LSP range (0-based) to Monaco's (1-based). */
function toRange(r: Json): monaco.IRange {
  return {
    startLineNumber: r.start.line + 1,
    startColumn: r.start.character + 1,
    endLineNumber: r.end.line + 1,
    endColumn: r.end.character + 1,
  };
}

/** MarkupContent, MarkedString or a plain string, as Monaco markdown. */
function toMarkdown(content: Json): monaco.IMarkdownString {
  if (typeof content === 'string') return { value: content };
  if (content.kind === 'plaintext')
    return { value: content.value.replace(/[\\`*_{}[\]()#+\-.!|<>]/g, '\\$&') };
  if (content.kind === 'markdown') return { value: content.value };
  if (content.language) return { value: `\`\`\`${content.language}\n${content.value}\n\`\`\`` };
  return { value: String(content.value ?? '') };
}

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
