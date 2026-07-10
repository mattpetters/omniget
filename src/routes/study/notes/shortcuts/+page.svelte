<script lang="ts">
  import { onMount } from "svelte";
  import PageHero from "$lib/study-components/PageHero.svelte";

  let isMac = $state(false);

  onMount(() => {
    if (typeof navigator !== "undefined") {
      isMac = /Mac|iPhone|iPad|iPod/i.test(navigator.platform || navigator.userAgent || "");
    }
  });

  const meta = $derived(isMac ? "Cmd" : "Ctrl");

  type Row = { keys: string[]; desc: string };
  type Section = { title: string; rows: Row[] };

  const SECTIONS = $derived<Section[]>([
    {
      title: "Structural editing",
      rows: [
        { keys: ["Tab"], desc: "Indent (becomes a child of the previous block)" },
        { keys: ["Shift+Tab"], desc: "Outdent (move up one level)" },
        { keys: ["Alt+↑"], desc: "Move block up" },
        { keys: ["Alt+↓"], desc: "Move block down" },
        { keys: [`${meta}+Shift+K`], desc: "Delete block (with confirmation)" },
        { keys: [`${meta}+/`], desc: "Collapse/expand block" },
        { keys: [`${meta}+D`], desc: "Duplicate block (with the whole subtree)" },
      ],
    },
    {
      title: "Status TODO",
      rows: [
        { keys: [`${meta}+Enter`], desc: "Cycle status (TODO → DOING → DONE → empty)" },
      ],
    },
    {
      title: "Inline formatting",
      rows: [
        { keys: [`${meta}+B`], desc: "Bold (`**text**`)" },
        { keys: [`${meta}+I`], desc: "Italic (`_text_`)" },
        { keys: [`${meta}+Shift+S`], desc: "Strikethrough (`~~text~~`)" },
        { keys: [`${meta}+Shift+C`], desc: "Inline code (`` `text` ``)" },
        { keys: [`${meta}+Shift+.`], desc: "Blockquote (`> ` on the line)" },
      ],
    },
    {
      title: "Insert via slash menu",
      rows: [
        { keys: ["/"], desc: "Open slash menu (15 commands)" },
        { keys: ["/todo /doing /done /later /now /waiting /canceled"], desc: "Set block status" },
        { keys: ["/today"], desc: "Insert today's ISO date" },
        { keys: ["/date"], desc: "Insert today's journal link [[YYYY-MM-DD]]" },
        { keys: ["/page /tag /block"], desc: "Start [[, #, or ((" },
        { keys: ["/code"], desc: "Insert code block ``` ```" },
        { keys: ["/query"], desc: "Insert {{query (and (todo TODO))}} skeleton" },
        { keys: ["/embed page", "/embed block"], desc: "Insert {{embed [[…]]}} or {{embed ((…))}}" },
      ],
    },
    {
      title: "Autocomplete inline",
      rows: [
        { keys: ["[["], desc: "Autocomplete existing pages" },
        { keys: ["#"], desc: "Autocomplete tags" },
        { keys: ["(("], desc: "Autocomplete recent blocks (uuid)" },
      ],
    },
    {
      title: "History",
      rows: [
        { keys: [`${meta}+Z`], desc: "Undo the last block content edit" },
        { keys: [`${meta}+Alt+Z`], desc: "Undo the last structural operation (move/delete/insert)" },
        { keys: [`${meta}+Shift+Z`, `${meta}+Y`], desc: "Redo the last structural operation" },
      ],
    },
    {
      title: "Exit",
      rows: [
        { keys: ["Esc"], desc: "Close autocomplete / cancel selection" },
      ],
    },
    {
      title: "Markdown syntax recognized in preview",
      rows: [
        { keys: ["`> [!note]` `[!warn]` `[!info]` `[!success]` `[!tip]`"], desc: "Colored callout below the block" },
        { keys: ["` ```lang `\\n`code`\\n` ``` `"], desc: "Syntax-highlighted code block (preview)" },
        { keys: ["`$math$` or `$$display$$`"], desc: "LaTeX rendered via KaTeX (preview)" },
        { keys: ["`| col1 | col2 |`\\n`|---|---|`\\n`|...|...|`"], desc: "Markdown table rendered below" },
        { keys: ["`{{query (...)}}` `:sort X :limit N :offset M`"], desc: "Inline query with live table and pagination" },
      ],
    },
    {
      title: "Search syntax",
      rows: [
        { keys: ["`tag:project`"], desc: "Filter blocks with link [[project]] or #project" },
        { keys: ["`page:Daily`"], desc: "Filter blocks from the Daily page" },
        { keys: ["`status:DOING`"], desc: "Filter by status property" },
        { keys: ["`before:2026-05-01`", "`after:2026-04-01`"], desc: "updated_at window" },
        { keys: ["`tag:\"two words\"`"], desc: "Use quotes for values with spaces" },
      ],
    },
  ]);
</script>

<section class="shortcuts-page">
  <PageHero
    title="Notes editor shortcuts"
    subtitle="Detected: {isMac ? 'Mac' : 'Windows/Linux'} ({meta} = {meta})"
  />

  <p class="muted small">
    This page is static. Every shortcut listed here is wired in the editor at
    <code>/study/notes</code>. If something does not work, it is a bug.
  </p>

  {#each SECTIONS as section (section.title)}
    <section class="sec">
      <h2>{section.title}</h2>
      <table class="sc-table">
        <tbody>
          {#each section.rows as row (row.desc)}
            <tr>
              <td class="keys-cell">
                {#each row.keys as k, i (i)}
                  {#if i > 0} or {/if}
                  {#each k.split("+") as part, j (j)}
                    {#if j > 0}<span class="plus">+</span>{/if}
                    <kbd>{part}</kbd>
                  {/each}
                {/each}
              </td>
              <td class="desc-cell">{row.desc}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </section>
  {/each}
</section>

<style>
  .shortcuts-page {
    display: flex;
    flex-direction: column;
    gap: calc(var(--padding) * 1.25);
    width: 100%;
    max-width: 880px;
    margin-inline: auto;
  }
  .muted {
    color: var(--tertiary);
  }
  .small {
    font-size: 12px;
  }
  .sec {
    background: var(--surface);
    border: 1px solid color-mix(in oklab, var(--input-border) 60%, transparent);
    border-radius: var(--border-radius);
    padding: calc(var(--padding) * 0.9);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .sec h2 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--accent);
  }
  .sc-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  .sc-table td {
    padding: 6px 8px;
    border-bottom: 1px solid color-mix(in oklab, var(--input-border) 40%, transparent);
    vertical-align: top;
  }
  .sc-table tr:last-child td {
    border-bottom: 0;
  }
  .keys-cell {
    width: 35%;
    white-space: nowrap;
  }
  .desc-cell {
    color: var(--secondary);
  }
  kbd {
    display: inline-block;
    padding: 2px 6px;
    background: var(--bg);
    border: 1px solid var(--input-border);
    border-bottom-width: 2px;
    border-radius: 4px;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    color: var(--text);
    line-height: 1;
  }
  .plus {
    margin: 0 2px;
    color: var(--tertiary);
    font-size: 11px;
  }
  code {
    padding: 1px 4px;
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    border-radius: 3px;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
  }
</style>
