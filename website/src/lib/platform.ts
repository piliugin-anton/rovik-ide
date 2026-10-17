// Which desktop the visitor is on, to offer its download first. Runs in the
// browser only (after hydration).

export type Platform = 'linux' | 'macos' | 'windows';
export type Arch = 'x64' | 'arm64';

export interface Visitor {
  /** null on a phone or tablet, or a system we can't tell. */
  platform: Platform | null;
  /** null when the browser doesn't say. */
  arch: Arch | null;
}

export const PLATFORM_NAMES: Record<Platform, string> = {
  linux: 'Linux',
  macos: 'macOS',
  windows: 'Windows',
};

interface UserAgentData {
  platform?: string;
  getHighEntropyValues?(hints: string[]): Promise<{ architecture?: string }>;
}

export async function detectVisitor(): Promise<Visitor> {
  const nav = navigator as Navigator & { userAgentData?: UserAgentData };
  const ua = nav.userAgent;
  const hints = `${nav.userAgentData?.platform ?? ''} ${nav.platform ?? ''} ${ua}`;

  let platform: Platform | null = null;
  if (/android|iphone|ipad|ipod/i.test(ua)) platform = null;
  // iPadOS asks for desktop pages as a Mac; a touch screen gives it away.
  else if (/mac/i.test(hints)) platform = nav.maxTouchPoints > 1 ? null : 'macos';
  else if (/win/i.test(hints)) platform = 'windows';
  else if (/linux|x11|cros/i.test(hints)) platform = 'linux';

  let arch: Arch | null = null;
  try {
    const values = await nav.userAgentData?.getHighEntropyValues?.(['architecture']);
    if (values?.architecture === 'arm') arch = 'arm64';
    else if (values?.architecture === 'x86') arch = 'x64';
  } catch {
    // Not offered: the default for the platform applies.
  }
  if (!arch && /aarch64|arm64/i.test(ua)) arch = 'arm64';
  return { platform, arch };
}
