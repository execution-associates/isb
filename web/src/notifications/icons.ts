// One icon per channel destination, for the cards and the channel dialog.
import { type Bell, Hash, Mail, MessageCircle, Send, Webhook } from "lucide-react";
import type { ProviderType } from "./api";

export const PROVIDER_ICON: Record<ProviderType, typeof Bell> = {
  webhook: Webhook,
  slack: Hash,
  discord: MessageCircle,
  telegram: Send,
  email: Mail,
};
