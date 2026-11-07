<script lang="ts">
  interface Props {
    value: string;
    label?: string;
    type?: string;
    placeholder?: string;
    id?: string;
    name?: string;
    autocomplete?: AutoFill;
    required?: boolean;
    disabled?: boolean;
    error?: string;
    hint?: string;
    prefix?: string;
    /** Which on-screen keyboard a phone shows (e.g. `numeric` for day counts). */
    inputmode?: 'text' | 'numeric' | 'decimal' | 'email' | 'tel' | 'url' | 'search';
    oninput?: (event: Event) => void;
  }

  let {
    value = $bindable(),
    label,
    type = 'text',
    placeholder,
    id,
    name,
    autocomplete,
    required = false,
    disabled = false,
    error,
    hint,
    prefix,
    inputmode,
    oninput,
  }: Props = $props();

  const generatedId = `f-${Math.random().toString(36).slice(2, 9)}`;
  const fieldId = $derived(id ?? generatedId);
</script>

<div class="field">
  {#if label}
    <label class="lbl" for={fieldId}>
      {label}{#if required}<span class="req">*</span>{/if}
    </label>
  {/if}
  <div class="control" class:has-error={!!error} class:has-prefix={!!prefix}>
    {#if prefix}<span class="prefix mono">{prefix}</span>{/if}
    <input
      id={fieldId}
      {name}
      {type}
      {placeholder}
      {required}
      {disabled}
      {autocomplete}
      {inputmode}
      bind:value
      oninput={oninput}
    />
  </div>
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
  .req {
    color: var(--error);
    margin-inline-start: 2px;
  }
  .control {
    display: flex;
    align-items: center;
    min-height: var(--control-h);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    transition: border-color 0.12s ease, box-shadow 0.12s ease;
  }
  .control:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-soft);
  }
  .control.has-error {
    border-color: var(--error);
  }
  .control.has-error:focus-within {
    box-shadow: 0 0 0 3px var(--error-soft);
  }
  .prefix {
    padding-inline-start: 10px;
    color: var(--text-faint);
    font-size: var(--text-sm);
  }
  input {
    flex: 1;
    width: 100%;
    padding: 0 10px;
    height: calc(var(--control-h) - 2px);
    background: transparent;
    border: none;
    color: var(--text);
    outline: none;
    min-width: 0;
  }
  input::placeholder {
    color: var(--text-faint);
  }
  input:disabled {
    cursor: not-allowed;
    opacity: 0.6;
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
