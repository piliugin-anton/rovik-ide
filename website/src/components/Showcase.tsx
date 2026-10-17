import AppWindow from './AppWindow';

// A screenshot of the app, added as src/assets/screenshot.png (or .jpg,
// .webp, .avif), takes the drawing's place.
const screenshots = import.meta.glob<string>('../assets/screenshot.{png,jpg,jpeg,webp,avif}', {
  eager: true,
  query: '?url',
  import: 'default',
});
const screenshot = Object.values(screenshots)[0];

export default function Showcase() {
  return (
    <div class="container showcase">
      <div class="showcase-glow" aria-hidden="true" />
      {screenshot ? (
        <img
          class="screenshot"
          src={screenshot}
          alt="Rovik IDE: the agent's chat in the sidebar beside the editor"
        />
      ) : (
        <AppWindow />
      )}
    </div>
  );
}
