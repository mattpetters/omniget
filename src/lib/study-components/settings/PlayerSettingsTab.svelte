<script lang="ts">
  import SettingsField from "./SettingsField.svelte";
  import SettingsSlider from "./SettingsSlider.svelte";
  import SettingsToggle from "./SettingsToggle.svelte";
  import type { StudySettings } from "$lib/study-bridge";

  type Props = {
    settings: StudySettings;
    onPatch: (patch: StudySettings) => void;
  };

  let { settings, onPatch }: Props = $props();

  function setPlayer<K extends keyof NonNullable<StudySettings["player"]>>(
    key: K,
    value: NonNullable<StudySettings["player"]>[K],
  ) {
    onPatch({ player: { ...(settings.player ?? {}), [key]: value } });
  }

  const player = $derived(settings.player ?? {});
</script>

<section class="tab">
  <SettingsField
    label="Completion threshold"
    description="Video percentage after which the lesson is marked complete"
    valueDisplay={`${Math.round((player.completion_threshold ?? 0.95) * 100)}%`}
  >
    <SettingsSlider
      value={Math.round((player.completion_threshold ?? 0.95) * 100)}
      min={50}
      max={100}
      step={1}
      onChange={(v) => setPlayer("completion_threshold", v / 100)}
    />
  </SettingsField>

  <SettingsField
    label="Long seek"
    description="Jump length for the J/L shortcut"
    valueDisplay={`${(player.seek_step_long_ms ?? 10000) / 1000}s`}
  >
    <SettingsSlider
      value={player.seek_step_long_ms ?? 10000}
      min={1000}
      max={30000}
      step={1000}
      onChange={(v) => setPlayer("seek_step_long_ms", v)}
    />
  </SettingsField>

  <SettingsField
    label="Short seek"
    description="Jump length for Shift+J/L"
    valueDisplay={`${(player.seek_step_short_ms ?? 3000) / 1000}s`}
  >
    <SettingsSlider
      value={player.seek_step_short_ms ?? 3000}
      min={1000}
      max={10000}
      step={500}
      onChange={(v) => setPlayer("seek_step_short_ms", v)}
    />
  </SettingsField>

  <SettingsField
    label="Pause when minimized"
    description="Pause the video when the window loses focus"
  >
    <SettingsToggle
      value={player.pause_on_minimize ?? false}
      onChange={(v) => setPlayer("pause_on_minimize", v)}
      ariaLabel="Pause when minimized"
    />
  </SettingsField>

  <SettingsField
    label="ESC exits fullscreen"
    description="When enabled, ESC exits fullscreen. When disabled, ESC closes the lesson"
  >
    <SettingsToggle
      value={player.esc_exit_fullscreen ?? true}
      onChange={(v) => setPlayer("esc_exit_fullscreen", v)}
      ariaLabel="ESC exits fullscreen"
    />
  </SettingsField>

  <SettingsField
    label="Hero blur intensity"
    description="Blur behind the title in the course page hero"
    valueDisplay={`${player.hero_blur_intensity ?? 40}`}
  >
    <SettingsSlider
      value={player.hero_blur_intensity ?? 40}
      min={0}
      max={100}
      step={5}
      onChange={(v) => setPlayer("hero_blur_intensity", v)}
    />
  </SettingsField>

  <SettingsField
    label="Generate thumbnails automatically"
    description="Creates a VTT sprite for progress-bar previews (uses disk space)"
  >
    <SettingsToggle
      value={player.thumbnails_auto_generate ?? false}
      onChange={(v) => setPlayer("thumbnails_auto_generate", v)}
      ariaLabel="Generate thumbnails automatically"
    />
  </SettingsField>
</section>

<style>
  .tab {
    display: flex;
    flex-direction: column;
  }
</style>
