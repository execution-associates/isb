import { Monitor, Moon, Palette, Sun } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { setTheme, type Theme, useTheme } from "@/lib/theme";

/** Every theme, in the order menus list them; the first is the default. */
export const THEMES: { value: Theme; label: string; icon: typeof Sun }[] = [
  { value: "ea", label: "Execution Associates", icon: Palette },
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "system", label: "System", icon: Monitor },
];

/** Each theme's icon, for the account menu's Theme item. */

export const THEME_ICONS: Record<Theme, typeof Sun> = { ea: Palette, light: Sun, dark: Moon, system: Monitor };

export function ThemeToggle() {
  const { theme, effective } = useTheme();
  const Icon = theme === "ea" ? Palette : effective === "dark" ? Moon : Sun;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label="Theme">
          <Icon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuRadioGroup value={theme} onValueChange={(v) => setTheme(v as Theme)}>
          {THEMES.map((t) => (
            <DropdownMenuRadioItem key={t.value} value={t.value}>
              <t.icon className="size-4" />
              {t.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
