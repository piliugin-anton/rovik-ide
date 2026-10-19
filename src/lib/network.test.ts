// Unit tests: Settings → Network's helpers (lib/network.ts): a proxy's
// address taken apart and put together, this computer's proxy as its line
// says it, each kind of connection's menu and what its options say, the
// proxies' ids and uses, and the Settings menu's line. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DEFAULT_ROUTES,
  describeProxyUrl,
  networkSummary,
  newProxyId,
  portOf,
  proxyAddress,
  proxyLabel,
  proxyUses,
  routeOf,
  routeValue,
  rowOptions,
  splitProxyAddress,
  systemProxyLine,
  throughProxy,
  type NetworkSettings,
  type NetworkView,
  type Route,
  type SavedProxy,
} from './network';

const HTTP: SavedProxy = {
  id: 'p1',
  kind: 'http',
  host: 'proxy.corp',
  port: 3128,
  username: 'alice',
  passwordSaved: true,
};
const SOCKS: SavedProxy = { id: 's1', kind: 'socks5', host: '10.0.0.2', port: 1080 };
const CORP = { https: 'http://proxy.corp:3128', http: 'http://proxy.corp:3128' };

function view(
  settings: Partial<NetworkSettings> = {},
  system: NetworkView['system'] = {}
): NetworkView {
  return {
    settings: { proxies: [], routes: { ...DEFAULT_ROUTES }, ...settings },
    system,
    defaultTimeoutSecs: 30,
    minTimeoutSecs: 5,
    maxTimeoutSecs: 300,
  };
}

test('a proxy typed or pasted is taken apart into its kind, host and port', () => {
  assert.deepEqual(splitProxyAddress(' proxy.example.com '), {
    host: 'proxy.example.com',
    kind: undefined,
  });
  assert.deepEqual(splitProxyAddress('proxy.example.com:3128'), {
    host: 'proxy.example.com',
    port: 3128,
    kind: undefined,
  });
  assert.deepEqual(splitProxyAddress('http://proxy.example.com:3128/'), {
    host: 'proxy.example.com',
    port: 3128,
    kind: 'http',
  });
  assert.deepEqual(splitProxyAddress('HTTPS://proxy.example.com:8443'), {
    host: 'proxy.example.com',
    port: 8443,
    kind: 'http',
  });
  for (const socks of [
    'socks5://10.0.0.2:1080',
    'socks5h://10.0.0.2:1080',
    'socks://10.0.0.2:1080',
  ]) {
    assert.deepEqual(
      splitProxyAddress(socks),
      { host: '10.0.0.2', port: 1080, kind: 'socks5' },
      socks
    );
  }
  assert.deepEqual(splitProxyAddress('[::1]:8080'), { host: '::1', port: 8080, kind: undefined });
  assert.deepEqual(splitProxyAddress('::1'), { host: '::1', kind: undefined });
  // What isn't a proxy's URL stays as typed, for the backend to say why.
  assert.deepEqual(splitProxyAddress('ftp://files.example.com'), {
    host: 'ftp://files.example.com',
  });
  assert.deepEqual(splitProxyAddress('proxy:99999'), {
    host: 'proxy',
    port: undefined,
    kind: undefined,
  });
});

test('a port is a whole number from 1 to 65535', () => {
  assert.equal(portOf('3128'), 3128);
  assert.equal(portOf(' 80 '), 80);
  for (const wrong of ['', '0', '65536', '1.5', 'eighty', '-1'])
    assert.equal(portOf(wrong), undefined, wrong);
});

test('a proxy is said by its kind, its address, an IPv6 one in brackets, and its user', () => {
  assert.equal(proxyAddress({ host: 'proxy.example.com', port: 3128 }), 'proxy.example.com:3128');
  assert.equal(proxyAddress({ host: '::1', port: 1080 }), '[::1]:1080');
  assert.equal(proxyAddress({ host: '[::1]', port: 1080 }), '[::1]:1080');
  assert.equal(proxyLabel(HTTP), 'HTTP proxy.corp:3128 (alice)');
  assert.equal(proxyLabel(SOCKS), 'SOCKS5 10.0.0.2:1080');
});

test('this computer’s proxy is said as its address and kind', () => {
  assert.equal(describeProxyUrl('http://proxy.corp:3128'), 'proxy.corp:3128 (HTTP)');
  assert.equal(describeProxyUrl('https://proxy.corp:443/'), 'proxy.corp:443 (HTTPS)');
  assert.equal(describeProxyUrl('socks5h://socks.corp:1080'), 'socks.corp:1080 (SOCKS5)');
  assert.equal(systemProxyLine(view()), 'none set');
  assert.equal(systemProxyLine(view({}, CORP)), 'proxy.corp:3128 (HTTP)');
  assert.equal(
    systemProxyLine(view({}, { https: 'http://secure.corp:3128', http: 'http://plain.corp:8080' })),
    'secure.corp:3128 (HTTP) for HTTPS, plain.corp:8080 (HTTP) for HTTP'
  );
  assert.equal(
    systemProxyLine(view({}, { http: 'http://plain.corp:8080' })),
    'plain.corp:8080 (HTTP)'
  );
});

test('a route and its option value are one another', () => {
  const routes: Route[] = ['auto', 'own', 'direct', 'system', { proxy: 'p1' }, { proxy: 'a-b_9' }];
  for (const route of routes) assert.deepEqual(routeOf(routeValue(route)), route);
  assert.equal(routeValue({ proxy: 'p1' }), 'proxy:p1');
});

test('each kind of connection’s menu has only what it takes, then the saved proxies', () => {
  const v = view({ proxies: [HTTP, SOCKS] });
  const values = (row: keyof NetworkSettings['routes']) => rowOptions(v, row).map((o) => o.value);
  const saved = ['proxy:p1', 'proxy:s1'];
  assert.deepEqual(values('models'), ['auto', 'direct', 'system', ...saved]);
  assert.deepEqual(values('ssh'), ['direct', 'system', ...saved]);
  assert.deepEqual(values('browser'), ['own', 'direct', 'system', ...saved]);
  assert.deepEqual(values('hostModels'), ['own', 'direct', ...saved]);
  assert.deepEqual(values('hostBrowser'), ['own', 'direct', ...saved]);

  assert.deepEqual(
    rowOptions(v, 'models').map((o) => o.label),
    [
      'Automatic (no proxy)',
      'No proxy',
      'System proxy',
      'HTTP proxy.corp:3128 (alice)',
      'SOCKS5 10.0.0.2:1080',
    ]
  );
  assert.equal(rowOptions(view({}, CORP), 'models')[0].label, 'Automatic (system proxy)');
  assert.equal(rowOptions(v, 'browser')[0].label, 'Its own settings');
  assert.equal(rowOptions(v, 'hostModels')[0].label, 'The host’s own settings');
  assert.equal(rowOptions(v, 'hostBrowser')[0].label, 'Its own settings');
});

test('a proxy’s uses are the kinds that go through it', () => {
  const settings: NetworkSettings = {
    proxies: [HTTP, SOCKS],
    routes: {
      ...DEFAULT_ROUTES,
      models: { proxy: 'p1' },
      hostBrowser: { proxy: 'p1' },
      ssh: { proxy: 's1' },
    },
  };
  assert.deepEqual(proxyUses(settings, 'p1'), ['models', 'hostBrowser']);
  assert.deepEqual(proxyUses(settings, 's1'), ['ssh']);
  assert.deepEqual(proxyUses(settings, 'other'), []);
});

test('a new proxy’s id is one no other has', () => {
  for (let i = 0; i < 50; i++) assert.match(newProxyId([]), /^proxy-[a-z0-9]{8}$/);
  // The first one drawn is taken: another is.
  const random = Math.random;
  let draws = 0;
  Math.random = () => (draws++ < 8 ? 0.5 : random());
  try {
    const taken = 'proxy-' + 'i'.repeat(8);
    const id = newProxyId([taken]);
    assert.notEqual(id, taken);
    assert.match(id, /^proxy-[a-z0-9]{8}$/);
  } finally {
    Math.random = random;
  }
});

test('what goes through a proxy now: a saved one, or this computer’s while it has one', () => {
  const v = (routes: Partial<NetworkSettings['routes']>, system = {}) =>
    view({ proxies: [HTTP], routes: { ...DEFAULT_ROUTES, ...routes } }, system);
  assert.equal(throughProxy(v({}), 'models'), false, 'automatic, none set');
  assert.equal(throughProxy(v({}, CORP), 'models'), true, 'automatic, one set');
  assert.equal(throughProxy(v({ ssh: 'system' }), 'ssh'), false);
  assert.equal(throughProxy(v({ ssh: 'system' }, CORP), 'ssh'), true);
  assert.equal(throughProxy(v({ browser: 'own' }, CORP), 'browser'), false, 'its own settings');
  assert.equal(throughProxy(v({ models: 'direct' }, CORP), 'models'), false);
  assert.equal(throughProxy(v({ hostModels: { proxy: 'p1' } }), 'hostModels'), true);
  assert.equal(throughProxy(v({ hostModels: { proxy: 'gone' } }), 'hostModels'), false);
});

test('the menu’s line says which kinds go through a proxy, and a timeout set', () => {
  assert.equal(networkSummary(view()), 'No proxy: connections go directly');
  assert.equal(networkSummary(view({}, CORP)), 'Through a proxy: model providers');
  assert.equal(
    networkSummary(view({ routes: { ...DEFAULT_ROUTES, models: 'direct' } }, CORP)),
    'No proxy: connections go directly'
  );
  const all: NetworkSettings['routes'] = {
    models: { proxy: 'p1' },
    ssh: 'system',
    browser: { proxy: 's1' },
    hostModels: { proxy: 'p1' },
    hostBrowser: { proxy: 's1' },
  };
  assert.equal(
    networkSummary(view({ proxies: [HTTP, SOCKS], routes: all }, CORP)),
    'Through a proxy: model providers, SSH to cloud hosts, the browser, cloud hosts’ model providers, cloud hosts’ browser'
  );
  // "System proxy" while none is set goes directly.
  assert.equal(
    networkSummary(
      view({ proxies: [HTTP], routes: { ...all, browser: 'system', hostBrowser: 'own' } })
    ),
    'Through a proxy: model providers, cloud hosts’ model providers'
  );
  assert.equal(
    networkSummary(view({ timeoutSecs: 60 })),
    'No proxy: connections go directly · 60 s to connect'
  );
  assert.equal(networkSummary(view({ timeoutSecs: 30 })), 'No proxy: connections go directly');
});
