// Exercise the real worker entry and Monaco's message handshake. Constructing
// ProjectTypeScriptWorker directly misses the order Monaco creates its host in.
import './testSelf';
import './worker';
import { it } from 'node:test';
import assert from 'node:assert/strict';

it(
  'starts through Monaco and answers imported hover, completion and definition requests',
  { timeout: 5_000 },
  async (t) => {
    const originalPostMessage = self.postMessage;
    const replies = new Map<
      string,
      { resolve(value: unknown): void; reject(error: unknown): void }
    >();
    self.postMessage = ((message: unknown) => {
      const { seq, res, err } = structuredClone(message) as {
        seq: string;
        res: unknown;
        err?: unknown;
      };
      const reply = replies.get(seq);
      assert.ok(reply, `unexpected reply ${seq}`);
      replies.delete(seq);
      if (err) reply.reject(err);
      else reply.resolve(res);
    }) as typeof self.postMessage;
    t.after(() => {
      self.postMessage = originalPostMessage;
    });

    const send = (data: unknown) => {
      assert.ok(self.onmessage);
      self.onmessage(new MessageEvent('message', { data: structuredClone(data) }));
    };
    let sequence = 0;
    const request = <T>(method: string, ...args: unknown[]): Promise<T> => {
      const req = String(++sequence);
      return new Promise((resolve, reject) => {
        replies.set(req, { resolve: (value) => resolve(value as T), reject });
        send({ vsWorker: 1, type: 0, req, channel: 'default', method, args });
      });
    };

    const uri = 'file:///project/app.ts';
    const definition = 'file:///project/node_modules/example/index.d.ts';
    const text = "import { answer } from 'example';\nanswer.toFixed();";
    // The first message arms initialization; the second supplies createData.
    send('ignore');
    send({
      compilerOptions: { target: 99, module: 99, moduleResolution: 2, types: [] },
      extraLibs: {
        [definition]: { content: 'export declare const answer: number;', version: 1 },
      },
    });
    await request('$initialize', 1);
    await request('$acceptNewModel', {
      url: uri,
      lines: text.split('\n'),
      EOL: '\n',
      versionId: 1,
    });

    const position = text.lastIndexOf('answer');
    const hover = await request<{ displayParts: { text: string }[] }>(
      '$fmr',
      'getQuickInfoAtPosition',
      [uri, position]
    );
    assert.match(hover.displayParts.map((part) => part.text).join(''), /answer: number/);
    const definitions = await request<{ fileName: string }[]>('$fmr', 'getDefinitionAtPosition', [
      uri,
      position,
    ]);
    assert.deepEqual(
      definitions.map((entry) => entry.fileName),
      [definition]
    );
    const completions = await request<{ entries: { name: string }[] }>(
      '$fmr',
      'getCompletionsAtPosition',
      [uri, position + 'answer.'.length]
    );
    assert.ok(completions.entries.some((entry) => entry.name === 'toFixed'));
  }
);
