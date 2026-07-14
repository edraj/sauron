export type ToastKind = 'success' | 'error' | 'info';

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

class ToastStore {
  items = $state<Toast[]>([]);
  private seq = 0;

  push(message: string, kind: ToastKind = 'info', ttl = 4000): number {
    const id = ++this.seq;
    this.items = [...this.items, { id, kind, message }];
    if (ttl > 0 && typeof window !== 'undefined') {
      window.setTimeout(() => this.dismiss(id), ttl);
    }
    return id;
  }

  success(message: string): number {
    return this.push(message, 'success');
  }

  error(message: string): number {
    return this.push(message, 'error', 6000);
  }

  info(message: string): number {
    return this.push(message, 'info');
  }

  dismiss(id: number): void {
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toastStore = new ToastStore();
