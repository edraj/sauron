<!--
  The one multi-line field: `Input`'s label, shell, focus ring, error and hint,
  around a <textarea>. Three pages used to hand-copy that shell onto a raw
  textarea, and the copies had already drifted apart.
-->
<script lang="ts">
  interface Props {
    value: string;
    label?: string;
    /** A faint note after the label, e.g. "optional". */
    labelNote?: string;
    id?: string;
    name?: string;
    rows?: number;
    placeholder?: string;
    disabled?: boolean;
    error?: string;
    hint?: string;
    /** Monospace, for pasted keys and secrets. */
    mono?: boolean;
    spellcheck?: boolean;
    autocomplete?: AutoFill;
  }

  let {
    value = $bindable(),
    label,
    labelNote,
    id,
    name,
    rows = 3,
    placeholder,
    disabled = false,
    error,
    hint,
    mono = false,
    spellcheck,
    autocomplete,
  }: Props = $props();

  const generatedId = `t-${Math.random().toString(36).slice(2, 9)}`;
  const fieldId = $derived(id ?? generatedId);
</script>

<div class="field">
  {#if label}
    <label class="lbl" for={fieldId}>
      {label}{#if labelNote}<span class="note">{labelNote}</span>{/if}
    </label>
  {/if}
  <textarea
    id={fieldId}
    {name}
    {rows}
    {placeholder}
    {disabled}
    {spellcheck}
    {autocomplete}
    class:mono
    class:has-error={!!error}
    bind:value
  ></textarea>
  {#if error}
    <span class="msg err">{error}</span>
  {:else if hint}
    <span class="msg hint">{hint}</span>
  {/if}
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    color: var(--text-muted);
  }
  .note {
    margin-inline-start: 6px;
    font-weight: var(--weight-regular);
    color: var(--text-faint);
  }
  textarea {
    width: 100%;
    min-height: 62px;
    padding: 7px 10px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-family: inherit;
    font-size: var(--text-sm);
    line-height: 1.5;
    resize: vertical;
    outline: none;
    transition: border-color 0.12s ease, box-shadow 0.12s ease;
  }
  textarea:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  textarea.has-error {
    border-color: var(--error);
  }
  textarea.has-error:focus {
    box-shadow: 0 0 0 3px var(--error-soft);
  }
  textarea::placeholder {
    color: var(--text-faint);
  }
  textarea:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }
  textarea.mono {
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
  }
  .msg {
    font-size: var(--text-xs);
  }
  .msg.err {
    color: var(--error);
  }
  .msg.hint {
    color: var(--text-faint);
  }
</style>
