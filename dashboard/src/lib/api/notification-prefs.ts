/**
 * One thin wrapper per endpoint, on the `api/alerts.ts` template.
 *
 * `api` for `/v1/me/*` (bearer + the 401 refresh-and-replay interceptor);
 * `bareClient` for the unsubscribe POST, which is unauthenticated and must
 * never be retried through the refresh path.
 */
import { api, bareClient } from './client';
import type {
  NotificationQueueItem,
  NotificationSubscription,
  SubscriptionConditions,
  SubscriptionDelivery,
  SubscriptionKind,
} from '../models';

export async function listSubscriptions(): Promise<NotificationSubscription[]> {
  const { data } = await api.get<NotificationSubscription[]>(
    '/v1/me/notification-subscriptions',
  );
  return data;
}

export interface UpsertSubscriptionBody {
  scope_type: 'project' | 'app';
  scope_id: string;
  kind: SubscriptionKind;
  /** CATALOGUE environment ids. `[]` means every environment. */
  environment_ids: string[];
  conditions: Partial<SubscriptionConditions>;
  delivery: SubscriptionDelivery;
  throttle_seconds: number;
  quiet_start_min: number | null;
  quiet_end_min: number | null;
  quiet_tz: string;
}

export async function createSubscription(
  body: UpsertSubscriptionBody,
): Promise<NotificationSubscription> {
  // There is no `org_id` field and there never will be one: the server derives
  // the org from the scope itself.
  const { data } = await api.post<NotificationSubscription>(
    '/v1/me/notification-subscriptions',
    body,
  );
  return data;
}

/**
 * PATCH accepts strictly less than POST, and the difference has to live in the
 * type rather than in a comment. The server's `PatchSubscriptionReq` has no
 * `scope_type`, `scope_id` or `kind` field and the handler does not set
 * `deny_unknown_fields`, so a body carrying any of the three used to come back
 * 200 with the row untouched — a caller re-pointing a subscription at another
 * app saw a success toast and no change. Omitting them here makes that body
 * fail to compile instead. Scope and kind are immutable: to change either,
 * delete the subscription and create a new one.
 */
export type PatchSubscriptionBody = Partial<
  Omit<UpsertSubscriptionBody, 'scope_type' | 'scope_id' | 'kind'>
> & { enabled?: boolean };

export async function updateSubscription(
  id: string,
  body: PatchSubscriptionBody,
): Promise<NotificationSubscription> {
  const { data } = await api.patch<NotificationSubscription>(
    `/v1/me/notification-subscriptions/${id}`,
    body,
  );
  return data;
}

export async function deleteSubscription(id: string): Promise<void> {
  await api.delete(`/v1/me/notification-subscriptions/${id}`);
}

export async function listNotifications(limit = 50): Promise<NotificationQueueItem[]> {
  const { data } = await api.get<NotificationQueueItem[]>('/v1/me/notifications', {
    params: { limit },
  });
  return data;
}

/**
 * Always resolves for any token the server accepted the request for — the
 * endpoint returns a generic 200 whether or not the token matched, so nothing
 * is disclosed about which subscription ids exist.
 */
export async function unsubscribe(token: string): Promise<void> {
  await bareClient.post('/v1/notifications/unsubscribe', { token });
}
