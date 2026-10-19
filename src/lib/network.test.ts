// Unit tests: Settings → Network's helpers (lib/network.ts): a proxy's
// address taken apart and put together, the system's proxy as its line says
// it, and the Settings menu's line. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  describeProxyUrl,
  networkSummary,
  portOf,
  proxyAddress,
  splitProxyAddress,
  systemProxyLine,
  type NetworkSettings,
  type NetworkView,
} from './network';

function view(
  settings: Partial<NetworkSettings> = {},
  system: NetworkView['system'] = {}
): NetworkView {
  return {
    settings: {
      proxy: 'direct',
      cloudHosts: false,
      browser: false,
      browserPlaces: 'local',
      ...settings,
    },
    passwordSaved: false,
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

test('an address puts an IPv6 host in brackets', () => {
  assert.equal(proxyAddress({ host: 'proxy.example.com', port: 3128 }), 'proxy.example.com:3128');
  assert.equal(proxyAddress({ host: '::1', port: 1080 }), '[::1]:1080');
  assert.equal(proxyAddress({ host: '[::1]', port: 1080 }), '[::1]:1080');
});

test('the system’s proxy is said as its address and kind', () => {
  assert.equal(describeProxyUrl('http://proxy.corp:3128'), 'proxy.corp:3128 (HTTP)');
  assert.equal(describeProxyUrl('https://proxy.corp:443/'), 'proxy.corp:443 (HTTPS)');
  assert.equal(describeProxyUrl('socks5h://socks.corp:1080'), 'socks.corp:1080 (SOCKS5)');
  assert.equal(systemProxyLine(view()), 'None is set: connections go directly.');
  const same = { https: 'http://proxy.corp:3128', http: 'http://proxy.corp:3128' };
  assert.equal(systemProxyLine(view({}, same)), 'proxy.corp:3128 (HTTP).');
  assert.equal(
    systemProxyLine(view({}, { https: 'http://secure.corp:3128', http: 'http://plain.corp:8080' })),
    'secure.corp:3128 (HTTP) for HTTPS, plain.corp:8080 (HTTP) for HTTP.'
  );
  assert.equal(
    systemProxyLine(view({}, { http: 'http://plain.corp:8080' })),
    'plain.corp:8080 (HTTP).'
  );
});

test('the menu’s line says the proxy in use, for cloud hosts too, and a timeout set', () => {
  const corp = { https: 'http://proxy.corp:3128' };
  const socks = { kind: 'socks5' as const, host: 'socks.example.com', port: 1080 };
  assert.equal(networkSummary(view()), 'No proxy: connections go directly');
  // No proxy: what's kept for one isn't said, the timeout is.
  assert.equal(
    networkSummary(view({ cloudHosts: true, browser: true, custom: socks, timeoutSecs: 60 }, corp)),
    'No proxy: connections go directly · 60 s to connect'
  );

  const system = { proxy: 'system' as const };
  assert.equal(networkSummary(view(system)), 'System proxy: none set, connections go directly');
  // Nothing to go through: cloud hosts aren't said to.
  assert.equal(
    networkSummary(view({ ...system, cloudHosts: true })),
    'System proxy: none set, connections go directly'
  );
  assert.equal(
    networkSummary(view({ ...system, cloudHosts: true }, corp)),
    'System proxy: proxy.corp:3128 (HTTP) · also for cloud hosts'
  );
  assert.equal(
    networkSummary(view({ proxy: 'custom', custom: socks, timeoutSecs: 60 })),
    'Custom SOCKS5 proxy: socks.example.com:1080 · 60 s to connect'
  );
  assert.equal(
    networkSummary(view({ proxy: 'custom', custom: socks, browser: true, browserPlaces: 'both' })),
    'Custom SOCKS5 proxy: socks.example.com:1080 · the browser too (local + cloud)'
  );
  // Kept while the system's is used: not said.
  assert.equal(
    networkSummary(view({ proxy: 'system', custom: socks, timeoutSecs: 30 })),
    'System proxy: none set, connections go directly'
  );
});
