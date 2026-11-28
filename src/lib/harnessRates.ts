import type { ModelRates } from '../types';

/** Convert a currency price per million tokens to exact stored microunits. */
export function parseHarnessRate(raw: string): number | undefined {
  const value = raw.trim();
  if (!/^(?:\d+(?:\.\d{0,6})?|\.\d{1,6})$/.test(value)) return undefined;
  const [whole, fraction = ''] = value.split('.');
  // Build an integer string so decimal prices never pass through floating-point multiplication.
  const parsed = Number(`${whole || '0'}${fraction.padEnd(6, '0')}`);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

/** Display stored microunits as a currency price without rounding or inventing missing values. */
export function formatHarnessRate(value: number | undefined): string {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) return '';
  const digits = String(value).padStart(7, '0');
  const fraction = digits.slice(-6).replace(/0+$/, '');
  return `${digits.slice(0, -6)}${fraction ? `.${fraction}` : ''}`;
}

/** A complete copy of the model's saved prices, without filling missing prices with zero. */
export function savedHarnessRates(
  profile: { harnessRates?: Partial<ModelRates> } | null | undefined
): ModelRates | null {
  const rates = profile?.harnessRates;
  if (
    !rates ||
    ![rates.input, rates.cachedInput, rates.output].every(
      (value) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
    )
  )
    return null;
  return { input: rates.input!, cachedInput: rates.cachedInput!, output: rates.output! };
}
