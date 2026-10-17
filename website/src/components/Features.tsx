import { For, type Component, type JSX } from 'solid-js';
import {
  ChecksIcon,
  CodeIcon,
  FeatherIcon,
  FolderTreeIcon,
  KeyIcon,
  MergeIcon,
  PlaneIcon,
  ServerIcon,
  SlidersIcon,
  TerminalIcon,
  UsersIcon,
} from './icons';

interface Feature {
  icon: Component<{ size?: number }>;
  title: string;
  text: string;
  wide?: boolean;
  extra?: () => JSX.Element;
}

// Autopilot's stages, as the strip above the message box shows them
// (Thorough adds its own between Checks and Acceptance).
const STAGES = [
  'Plan',
  'Plan review',
  'Tests',
  'Implement',
  'Change review',
  'Checks',
  'Acceptance',
  'Deliver',
];

const AGENT: Feature[] = [
  {
    icon: PlaneIcon,
    title: 'Autopilot',
    wide: true,
    text: 'One message, a whole task. Autopilot writes the criteria and a plan, has the plan reviewed, writes the tests first, then implements, reviews and checks the change and hands it over with a report. Nothing waits for you: what it can’t decide, it writes down. Pick Balanced or Thorough effort.',
    extra: () => (
      <ol class="stages" aria-label="Autopilot's stages">
        <For each={STAGES}>{(stage) => <li>{stage}</li>}</For>
      </ol>
    ),
  },
  {
    icon: ChecksIcon,
    title: 'Checked before it lands',
    text: 'When the agent is done, Rovik runs your type check, lint and tests. Failures go back to the agent, a fix that breaks what passed is rolled back, and weakened tests are flagged instead of counted.',
  },
  {
    icon: MergeIcon,
    title: 'Parallel chats, no collisions',
    text: 'Each chat works on its own copy of your changes. When it’s done, its work is merged into the project; conflicts come to you, and a landing can be undone.',
  },
  {
    icon: SlidersIcon,
    title: 'You stay in control',
    text: 'Choose the commands the agent may run and what needs your approval. Edits show as diffs you accept or reject change by change, and a second model can approve the routine ones.',
  },
  {
    icon: KeyIcon,
    title: 'Any model, your key',
    text: 'OpenAI, Anthropic, Google, OpenRouter, Fireworks or any other OpenAI-compatible API, with your own key. Or a local model in Ollama or LM Studio.',
  },
  {
    icon: UsersIcon,
    title: 'Subagents',
    text: 'Optional helpers with a fresh context: scouts that explore a question, a change reviewer, a test writer that never reads the new code, and step workers for long plans.',
  },
  {
    icon: ServerIcon,
    title: 'Cloud hosts',
    wide: true,
    text: 'Move a chat to your own Linux server over SSH and it keeps running while your laptop sleeps. A terminal on the host is one click away, and a team can share a server, each person in their own account.',
  },
];

const EDITOR: Feature[] = [
  {
    icon: FeatherIcon,
    title: 'Native and light',
    text: 'Built with Tauri and Rust on your system’s own web view, not a bundled Chromium: far less memory than Electron-based editors.',
  },
  {
    icon: CodeIcon,
    title: 'Monaco and IntelliSense',
    text: 'VS Code’s editor, with IntelliSense that understands your whole project and jumps to definitions across files.',
  },
  {
    icon: TerminalIcon,
    title: 'Terminal, search and Git',
    text: 'Your own shell, search across the project on ripgrep’s engine, and a Git panel with a side-by-side conflict resolver.',
  },
  {
    icon: FolderTreeIcon,
    title: 'Fast on big projects',
    text: 'The file tree draws only the rows you see, so 50,000 files cost about what 50 do, and it follows changes on disk.',
  },
];

function Card(props: { feature: Feature }) {
  const Icon = props.feature.icon;
  return (
    <li class="card" classList={{ 'card-wide': props.feature.wide }}>
      <span class="card-icon">
        <Icon size={20} />
      </span>
      <h3>{props.feature.title}</h3>
      <p>{props.feature.text}</p>
      {props.feature.extra?.()}
    </li>
  );
}

export default function Features() {
  return (
    <>
      <section class="section" id="features">
        <div class="container">
          <header class="section-head">
            <p class="eyebrow">The agent</p>
            <h2>Hand off the task, keep the control</h2>
            <p class="section-lead">
              The agent works in your project the way you would: it reads the code, makes the
              change, runs the checks and tells you what it did.
            </p>
          </header>
          <ul class="cards cards-agent">
            <For each={AGENT}>{(feature) => <Card feature={feature} />}</For>
          </ul>
        </div>
      </section>
      <section class="section section-tight">
        <div class="container">
          <header class="section-head">
            <p class="eyebrow">The editor</p>
            <h2>A fast editor underneath</h2>
            <p class="section-lead">
              Everything you need to read, write and ship code, in a native app that stays quick.
            </p>
          </header>
          <ul class="cards cards-editor">
            <For each={EDITOR}>{(feature) => <Card feature={feature} />}</For>
          </ul>
        </div>
      </section>
    </>
  );
}
