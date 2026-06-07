<script lang="ts">
  import { t } from "$lib/i18n";

  type Props = {
    open: boolean;
    onClose: () => void;
  };

  let { open, onClose }: Props = $props();

  type Shortcut = { keys: string[]; label: string };
  type Group = { title: string; items: Shortcut[] };

  const groups: Group[] = $derived([
    {
      title: $t("study.player_keyboardhelpoverlay.group_playback") as string,
      items: [
        { keys: ["Space"], label: $t("study.player_keyboardhelpoverlay.play_pause") as string },
        { keys: ["F"], label: $t("study.player_keyboardhelpoverlay.fullscreen") as string },
        { keys: ["M"], label: $t("study.player_keyboardhelpoverlay.mute_unmute") as string },
        { keys: ["T"], label: $t("study.player_keyboardhelpoverlay.theater_mode") as string },
        { keys: ["Esc"], label: $t("study.player_keyboardhelpoverlay.exit_fullscreen_close") as string },
      ],
    },
    {
      title: $t("study.player_keyboardhelpoverlay.group_navigation") as string,
      items: [
        { keys: ["←", "J"], label: $t("study.player_keyboardhelpoverlay.back_10s") as string },
        { keys: ["→", "L", "K"], label: $t("study.player_keyboardhelpoverlay.forward_10s") as string },
        { keys: ["Shift", "+", "J"], label: $t("study.player_keyboardhelpoverlay.back_3s") as string },
        { keys: ["Shift", "+", "L"], label: $t("study.player_keyboardhelpoverlay.forward_3s") as string },
        { keys: [","], label: $t("study.player_keyboardhelpoverlay.prev_frame") as string },
        { keys: ["."], label: $t("study.player_keyboardhelpoverlay.next_frame") as string },
        { keys: ["0", "—", "9"], label: $t("study.player_keyboardhelpoverlay.seek_percent") as string },
      ],
    },
    {
      title: $t("study.player_keyboardhelpoverlay.group_speed") as string,
      items: [
        { keys: ["["], label: $t("study.player_keyboardhelpoverlay.decrease_speed") as string },
        { keys: ["]"], label: $t("study.player_keyboardhelpoverlay.increase_speed") as string },
      ],
    },
    {
      title: $t("study.player_keyboardhelpoverlay.group_subs_notes") as string,
      items: [
        { keys: ["C"], label: $t("study.player_keyboardhelpoverlay.next_subtitle") as string },
        { keys: ["N"], label: $t("study.player_keyboardhelpoverlay.add_note_timestamp") as string },
      ],
    },
    {
      title: $t("study.player_keyboardhelpoverlay.group_general") as string,
      items: [
        { keys: ["?"], label: $t("study.player_keyboardhelpoverlay.show_this_panel") as string },
        { keys: ["/"], label: $t("study.player_keyboardhelpoverlay.search_any_route") as string },
      ],
    },
  ]);

  function onBackdropKey(e: KeyboardEvent) {
    if (e.key === "Escape" || e.key === "?" || e.key === "/") {
      e.preventDefault();
      onClose();
    }
  }
</script>

{#if open}
  <div
    class="overlay"
    role="dialog"
    aria-modal="true"
    aria-label={$t("study.player_keyboardhelpoverlay.title") as string}
    tabindex="-1"
    onkeydown={onBackdropKey}
  >
    <button type="button" class="bg-btn" aria-label={$t("study.player_keyboardhelpoverlay.close") as string} onclick={onClose}></button>
    <div class="modal" role="document">
      <header class="head">
        <h2>{$t("study.player_keyboardhelpoverlay.title")}</h2>
        <button type="button" class="close" aria-label={$t("study.player_keyboardhelpoverlay.close") as string} onclick={onClose}>×</button>
      </header>
      <div class="body">
        {#each groups as g (g.title)}
          <section class="group">
            <h3>{g.title}</h3>
            <ul>
              {#each g.items as s, i (i)}
                <li>
                  <span class="keys">
                    {#each s.keys as k, j (j)}
                      {#if k === "+" || k === "—"}
                        <span class="sep">{k}</span>
                      {:else}
                        <kbd>{k}</kbd>
                      {/if}
                    {/each}
                  </span>
                  <span class="label">{s.label}</span>
                </li>
              {/each}
            </ul>
          </section>
        {/each}
      </div>
      <footer class="foot">
        {$t("study.player_keyboardhelpoverlay.footer_before")}<kbd>?</kbd> {$t("study.player_keyboardhelpoverlay.footer_or")} <kbd>Esc</kbd> {$t("study.player_keyboardhelpoverlay.footer_after")}
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 250;
    display: grid;
    place-items: center;
    background: color-mix(in oklab, black 60%, transparent);
    animation: fade-in 180ms ease-out;
  }

  .bg-btn {
    position: absolute;
    inset: 0;
    background: transparent;
    border: none;
    cursor: default;
  }

  .modal {
    position: relative;
    width: min(640px, calc(100vw - 32px));
    max-height: calc(100vh - 64px);
    background: color-mix(in oklab, black 86%, transparent);
    backdrop-filter: blur(16px);
    border: 1px solid color-mix(in oklab, white 14%, transparent);
    border-radius: 14px;
    box-shadow: 0 24px 64px color-mix(in oklab, black 50%, transparent);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    color: white;
    z-index: 1;
    animation: pop-in 180ms ease-out;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px;
    border-bottom: 1px solid color-mix(in oklab, white 10%, transparent);
  }

  .head h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
  }

  .close {
    background: transparent;
    border: none;
    color: inherit;
    font-size: 24px;
    line-height: 1;
    padding: 4px 8px;
    border-radius: 6px;
    cursor: pointer;
  }

  .close:hover {
    background: color-mix(in oklab, white 12%, transparent);
  }

  .body {
    overflow-y: auto;
    padding: 16px 20px;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px 32px;
  }

  @media (max-width: 600px) {
    .body {
      grid-template-columns: 1fr;
    }
  }

  .group h3 {
    margin: 0 0 8px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-weight: 600;
    color: color-mix(in oklab, white 60%, transparent);
  }

  .group ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .group li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
  }

  .keys {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
  }

  .sep {
    color: color-mix(in oklab, white 50%, transparent);
    font-size: 11px;
  }

  kbd {
    display: inline-block;
    min-width: 22px;
    text-align: center;
    padding: 2px 6px;
    font-family: ui-monospace, monospace;
    font-size: 11px;
    font-weight: 600;
    background: color-mix(in oklab, white 14%, transparent);
    border: 1px solid color-mix(in oklab, white 18%, transparent);
    border-bottom-width: 2px;
    border-radius: 4px;
    color: white;
  }

  .label {
    color: color-mix(in oklab, white 80%, transparent);
    text-align: right;
  }

  .foot {
    padding: 10px 20px;
    border-top: 1px solid color-mix(in oklab, white 10%, transparent);
    text-align: center;
    font-size: 12px;
    color: color-mix(in oklab, white 60%, transparent);
  }

  .foot kbd {
    margin: 0 2px;
  }

  @keyframes fade-in {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }

  @keyframes pop-in {
    from {
      opacity: 0;
      transform: scale(0.96);
    }
    to {
      opacity: 1;
      transform: scale(1);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .overlay,
    .modal {
      animation: none;
    }
  }
</style>
