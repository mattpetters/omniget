<script lang="ts">
  import SettingsField from "./SettingsField.svelte";
  import SettingsSlider from "./SettingsSlider.svelte";
  import SettingsToggle from "./SettingsToggle.svelte";
  import { t } from "$lib/i18n";
  import type { StudySettings } from "$lib/study-bridge";

  type Props = {
    settings: StudySettings;
    onPatch: (patch: StudySettings) => void;
  };

  let { settings, onPatch }: Props = $props();
  const player = $derived(settings.player ?? {});

  function setPlayer<K extends keyof NonNullable<StudySettings["player"]>>(
    key: K,
    value: NonNullable<StudySettings["player"]>[K],
  ) {
    onPatch({ player: { ...(settings.player ?? {}), [key]: value } });
  }
</script>

<section class="tab">
  <SettingsField
    label={$t("study.settings_behaviorsettingstab.autoplay_label") as string}
    description={$t("study.settings_behaviorsettingstab.autoplay_desc") as string}
  >
    <SettingsToggle
      value={player.binge_watching ?? true}
      onChange={(v) => setPlayer("binge_watching", v)}
      ariaLabel={$t("study.settings_behaviorsettingstab.autoplay_aria") as string}
    />
  </SettingsField>

  <SettingsField
    label={$t("study.settings_behaviorsettingstab.countdown_label") as string}
    description={$t("study.settings_behaviorsettingstab.countdown_desc") as string}
    valueDisplay={`${(player.next_video_notification_ms ?? 5000) / 1000}s`}
  >
    <SettingsSlider
      value={player.next_video_notification_ms ?? 5000}
      min={1000}
      max={15000}
      step={500}
      onChange={(v) => setPlayer("next_video_notification_ms", v)}
    />
  </SettingsField>

  <SettingsField
    label={$t("study.settings_behaviorsettingstab.seek_logs_label") as string}
    description={$t("study.settings_behaviorsettingstab.seek_logs_desc") as string}
  >
    <SettingsToggle
      value={player.collect_seek_logs ?? true}
      onChange={(v) => setPlayer("collect_seek_logs", v)}
      ariaLabel={$t("study.settings_behaviorsettingstab.seek_logs_aria") as string}
    />
  </SettingsField>
</section>

<style>
  .tab {
    display: flex;
    flex-direction: column;
  }
</style>
