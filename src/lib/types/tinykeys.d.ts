// tinykeys ships type declarations, but its package.json `exports` map has no
// "types" condition, so TypeScript's bundler resolution can't see them.
// Mirror of the public API from tinykeys/dist/tinykeys.d.ts (v2.1).
declare module "tinykeys" {
  export type KeyBindingPress = [string[], string];

  export interface KeyBindingMap {
    [keybinding: string]: (event: KeyboardEvent) => void;
  }

  export interface KeyBindingHandlerOptions {
    timeout?: number;
  }

  export interface KeyBindingOptions extends KeyBindingHandlerOptions {
    event?: "keydown" | "keyup";
    capture?: boolean;
  }

  export function parseKeybinding(str: string): KeyBindingPress[];

  export function matchKeyBindingPress(
    event: KeyboardEvent,
    [mods, key]: KeyBindingPress,
  ): boolean;

  export function createKeybindingsHandler(
    keyBindingMap: KeyBindingMap,
    options?: KeyBindingHandlerOptions,
  ): EventListener;

  export function tinykeys(
    target: Window | HTMLElement,
    keyBindingMap: KeyBindingMap,
    options?: KeyBindingOptions,
  ): () => void;
}
