<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import {
    license,
    registrationUI,
    activateLicense,
  } from "$lib/license/state.svelte";
  import {
    SOLO_CHECKOUT_URL,
    PERSONAL_CHECKOUT_URL,
    SOLO_PRICE,
    PERSONAL_PRICE,
  } from "$lib/license/config";

  let anchor: HTMLDivElement | undefined = $state();
  let keyInput = $state("");
  let busy = $state(false);
  let error = $state("");
  // Registered users reveal the replace-key field on demand.
  let replacing = $state(false);

  const planLabel = $derived(
    license.plan === "solo"
      ? "Solo"
      : license.plan === "personal"
        ? "Personal"
        : "Licensed",
  );

  function toggle() {
    registrationUI.open = !registrationUI.open;
  }

  function close() {
    registrationUI.open = false;
    error = "";
    replacing = false;
  }

  async function register() {
    if (busy) return;
    busy = true;
    error = "";
    try {
      await activateLicense(keyInput);
      keyInput = "";
      replacing = false;
    } catch (e) {
      error = e instanceof Error ? e.message : "Something went wrong.";
    } finally {
      busy = false;
    }
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (registrationUI.open && event.key === "Escape") close();
  }

  function onWindowMousedown(event: MouseEvent) {
    if (
      registrationUI.open &&
      anchor &&
      !anchor.contains(event.target as Node)
    ) {
      close();
    }
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onmousedown={onWindowMousedown} />

<div class="anchor" bind:this={anchor}>
  {#if license.registered}
    <button
      class="badge registered"
      class:open={registrationUI.open}
      title="License"
      aria-label="License"
      onclick={toggle}
    >
      <Icon name="badgeCheck" size={15} />
    </button>
  {:else}
    <button
      class="badge unregistered"
      class:open={registrationUI.open}
      title="Register Jiji"
      onclick={toggle}
    >
      <Icon name="sparkles" size={13} />
      <span>Unregistered</span>
    </button>
  {/if}

  {#if registrationUI.open}
    <div class="panel" role="dialog" aria-label="Register Jiji">
      {#if license.registered && !replacing}
        <div class="head">
          <span class="head-icon"><Icon name="badgeCheck" size={18} /></span>
          <div class="head-text">
            <p class="title">Thanks for supporting Jiji 💛</p>
            <p class="sub">{planLabel} license · {license.displayKey}</p>
          </div>
        </div>
        <button class="link" onclick={() => (replacing = true)}>
          Replace license key
        </button>
      {:else}
        <p class="title">
          {license.registered ? "Replace your key" : "Support Jiji"}
        </p>
        <p class="copy">
          Jiji and every jj feature are free. A one-time license unlocks all ten
          themes and funds development.
        </p>

        <div class="field">
          <input
            class="key"
            type="text"
            spellcheck="false"
            autocomplete="off"
            placeholder="JIJI_XXXXXXXX-…"
            bind:value={keyInput}
            disabled={busy}
            onkeydown={(e) => e.key === "Enter" && register()}
          />
          <button
            class="register"
            onclick={register}
            disabled={busy || !keyInput.trim()}
          >
            {busy ? "Checking…" : "Register"}
          </button>
        </div>
        {#if error}<p class="error">{error}</p>{/if}

        {#if license.registered}
          <button
            class="link"
            onclick={() => {
              replacing = false;
              error = "";
            }}
          >
            Cancel
          </button>
        {:else}
          <div class="divider"><span>No key yet?</span></div>
          <div class="buy">
            <button class="buy-btn" onclick={() => openUrl(SOLO_CHECKOUT_URL)}>
              <span>Buy Solo</span>
              <span class="price">{SOLO_PRICE} · 1 device</span>
            </button>
            <button
              class="buy-btn popular"
              onclick={() => openUrl(PERSONAL_CHECKOUT_URL)}
            >
              <span>Buy Personal</span>
              <span class="price">{PERSONAL_PRICE} · 2 devices</span>
            </button>
          </div>
          <p class="fine">Secure checkout via Polar. Your key arrives by email.</p>
        {/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .anchor {
    position: relative;
  }

  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    border-radius: 999px;
    font-size: var(--text-s);
    font-weight: 500;
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .unregistered {
    padding: 0 11px 0 9px;
    color: var(--clr-accent-strong);
    background: color-mix(in srgb, var(--clr-accent) 14%, transparent);
  }

  .unregistered:hover,
  .unregistered.open {
    background: color-mix(in srgb, var(--clr-accent) 22%, transparent);
  }

  .registered {
    width: 30px;
    height: 30px;
    justify-content: center;
    padding: 0;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
  }

  .registered:hover,
  .registered.open {
    background: var(--clr-bg-hover);
    color: var(--clr-accent);
  }

  .panel {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 50;
    width: 320px;
    padding: var(--sp-4);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-l);
    box-shadow: var(--shadow-2);
    animation: pop var(--t-fast) var(--ease-out);
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(-4px) scale(0.98);
    }
  }

  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
  }

  .head-icon {
    color: var(--clr-accent);
    margin-top: 1px;
  }

  .title {
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .sub {
    margin-top: 2px;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .copy {
    margin-top: 5px;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-3);
  }

  .field {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .key {
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 10px;
    border-radius: var(--radius-m);
    background: var(--clr-bg-0);
    border: 1px solid var(--clr-border-1);
    color: var(--clr-text-1);
    font-family: var(--font-mono);
    font-size: var(--text-s);
  }

  .key:focus {
    outline: none;
    border-color: var(--clr-accent);
  }

  .register {
    flex-shrink: 0;
    height: 32px;
    padding: 0 14px;
    border-radius: var(--radius-m);
    background: var(--clr-accent);
    color: var(--clr-accent-contrast);
    font-size: var(--text-s);
    font-weight: 500;
    transition: background var(--t-fast) var(--ease-out);
  }

  .register:hover:not(:disabled) {
    background: var(--clr-accent-strong);
  }

  .register:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .error {
    margin-top: var(--sp-2);
    font-size: var(--text-s);
    line-height: 1.4;
    color: var(--clr-danger, #e5618a);
  }

  .divider {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: var(--sp-4) 0 var(--sp-3);
    color: var(--clr-text-3);
    font-size: var(--text-xs);
  }

  .divider::before,
  .divider::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--clr-border-2);
  }

  .buy {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-2);
  }

  .buy-btn {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 8px 11px;
    border-radius: var(--radius-m);
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-1);
    color: var(--clr-text-1);
    font-size: var(--text-s);
    font-weight: 500;
    transition:
      border-color var(--t-fast) var(--ease-out),
      background var(--t-fast) var(--ease-out);
  }

  .buy-btn:hover {
    border-color: var(--clr-accent);
  }

  .buy-btn.popular {
    border-color: color-mix(in srgb, var(--clr-accent) 50%, var(--clr-border-1));
  }

  .buy-btn .price {
    font-size: var(--text-xs);
    font-weight: 400;
    color: var(--clr-text-3);
  }

  .fine {
    margin-top: var(--sp-3);
    font-size: var(--text-xs);
    line-height: 1.45;
    color: var(--clr-text-3);
  }

  .link {
    margin-top: var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-accent-strong);
  }

  .link:hover {
    text-decoration: underline;
  }
</style>
