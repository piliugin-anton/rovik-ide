// src/lib/tools.ts — new file
import { toolListDir, toolSearchCode, toolReadFileRange, toolWriteFile } from './commands';

/**
 * Tool schemas in OpenAI's function-calling format, sent as the `tools`
 * field of the chat completion request. Paths are always relative to the
 * project root — the model never sees or sets the absolute root path; the
 * executor below fills that in from app state before invoking anything.
 */
export const AGENT_TOOL_SCHEMAS = [
  {
    type: 'function',
    function: {
      name: 'read_file',
      description:
        'Read a text file from the open project, optionally a specific 1-indexed inclusive line range. ' +
        'Full-file reads are truncated past ~20,000 characters — request a line range for large files.',
      parameters: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File path relative to the project root, e.g. "src/lib/foo.ts".',
          },
          start_line: {
            type: 'integer',
            description: 'First line to include (1-indexed). Omit to read from the start.',
          },
          end_line: {
            type: 'integer',
            description: 'Last line to include (1-indexed, inclusive). Omit to read to the end.',
          },
        },
        required: ['path'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'write_file',
      description:
        'Write content to a file in the project, creating it (and any missing parent directories) if it ' +
        'does not exist, or overwriting it if it does. There is no patch/diff tool — for edits to an ' +
        'existing file, read it first and write back the complete updated content.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'File path relative to the project root.' },
          content: { type: 'string', description: 'The complete new content of the file.' },
        },
        required: ['path', 'content'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'list_dir',
      description:
        'List files and directories under a path in the project, recursively up to a depth limit. ' +
        'Respects .gitignore and automatically skips node_modules/target/build-style directories and ' +
        'dotfiles. Use this before assuming a file does or does not exist.',
      parameters: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description:
              'Directory to list, relative to the project root. Omit to list from the root.',
          },
          max_depth: {
            type: 'integer',
            description: 'How many directory levels to descend (default 3, max 6).',
          },
        },
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'search_code',
      description:
        'Case-insensitive text search across project files, skipping ignored and binary files. Returns ' +
        'matching file paths, line numbers, and the matching line text. Use this to find where something ' +
        'is defined or used before reading whole files.',
      parameters: {
        type: 'object',
        properties: {
          query: { type: 'string', description: 'Text to search for.' },
          path_glob: {
            type: 'string',
            description:
              'Optional simple filter with a leading/trailing wildcard, e.g. "*.ts" or "src/*".',
          },
          max_results: {
            type: 'integer',
            description: 'Maximum number of matches to return (default 100, max 500).',
          },
        },
        required: ['query'],
      },
    },
  },
] as const;

/** Loosely-typed arguments as parsed from the model's JSON — validated per-tool below. */
type ToolArgs = Record<string, unknown>;

function parseArgs(argsJson: string): ToolArgs {
  if (!argsJson.trim()) return {};
  return JSON.parse(argsJson) as ToolArgs;
}

function asString(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined;
}

function asNumber(v: unknown): number | undefined {
  return typeof v === 'number' ? v : undefined;
}

/**
 * Executes a single tool call the model requested and returns the string to
 * send back as the `content` of the corresponding role:"tool" message.
 * Never throws — failures come back as an "Error: ..." string so the model
 * can see what went wrong and try something else, rather than the agent
 * loop crashing.
 */
export async function executeAgentTool(
  root: string,
  name: string,
  argsJson: string
): Promise<string> {
  let args: ToolArgs;
  try {
    args = parseArgs(argsJson);
  } catch {
    return `Error: could not parse arguments as JSON: ${argsJson}`;
  }

  try {
    switch (name) {
      case 'read_file': {
        const path = asString(args.path);
        if (!path) return 'Error: "path" is required.';
        return await toolReadFileRange(
          root,
          path,
          asNumber(args.start_line),
          asNumber(args.end_line)
        );
      }

      case 'write_file': {
        const path = asString(args.path);
        const content = asString(args.content);
        if (!path) return 'Error: "path" is required.';
        if (content === undefined) return 'Error: "content" is required.';
        return await toolWriteFile(root, path, content);
      }

      case 'list_dir': {
        const entries = await toolListDir(root, asString(args.path), asNumber(args.max_depth));
        if (entries.length === 0) return '(empty, or everything under this path is ignored)';
        return entries.map((e) => `${e.is_dir ? 'dir ' : 'file'}  ${e.path}`).join('\n');
      }

      case 'search_code': {
        const query = asString(args.query);
        if (!query) return 'Error: "query" is required.';
        const matches = await toolSearchCode(
          root,
          query,
          asString(args.path_glob),
          asNumber(args.max_results)
        );
        if (matches.length === 0) return 'No matches.';
        return matches.map((m) => `${m.path}:${m.line}: ${m.text}`).join('\n');
      }

      default:
        return `Error: unknown tool "${name}".`;
    }
  } catch (err) {
    return `Error: ${String(err)}`;
  }
}
