// connectorFields.ts
//
// A connector's form (ConnectorsSettings.tsx) is laid out the same for
// every kind: its name; a Connection section with how it signs in, then
// the test and Set up, the same for all; its own sections of options; and
// its models. What a kind brings is its `ConnectorFields`: the fields of
// its Connection section, and its options. A new kind adds its entry to
// `CONNECTOR_FIELDS`, beside its `ConnectorSpec` (lib/connectors.ts).
import type { Component } from 'solid-js';
import type { ConnectorKind, Provider } from '../types';
import { CLAUDE_CODE_FIELDS } from './ClaudeCodeFields';

/** What a kind's fields get: the same for every kind. */
export interface ConnectorFieldsProps {
  /** The connector being edited. */
  connector: Provider;
  /** A change to what its runs use: its settings, its key. */
  update: (changes: Partial<Provider>) => void;
}

/** A kind's part of its form. */
export interface ConnectorFields {
  /** In the Connection section, before its test and Set up: how it signs in. */
  Connection: Component<ConnectorFieldsProps>;
  /** Its own options, after Connection: each section a `settings-section`. */
  Options: Component<ConnectorFieldsProps>;
}

/** Each kind's fields: one entry per kind. */
export const CONNECTOR_FIELDS: Record<ConnectorKind, ConnectorFields> = {
  'claude-code': CLAUDE_CODE_FIELDS,
};
