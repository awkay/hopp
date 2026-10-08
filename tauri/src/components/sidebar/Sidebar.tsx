import React, { useEffect, useMemo, useState } from "react";
import throttle from "lodash/throttle";
import toast from "react-hot-toast";
import { openUrl } from "@tauri-apps/plugin-opener";
import { HiOutlineUsers, HiOutlineLockOpen, HiOutlineUserPlus, HiOutlineMinus } from "react-icons/hi2";
import { CgSpinner } from "react-icons/cg";
import { differenceInDays, parseISO } from "date-fns";
import { Separator } from "../ui/separator";
import { clsx } from "clsx";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { invoke } from "@tauri-apps/api/core";
import useStore, { Tab } from "@/store/store";
import { components } from "@/openapi";
import { HiOutlineAnnotation, HiOutlineDotsHorizontal, HiOutlineUserGroup } from "react-icons/hi";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { appVersion, isFloatingMainWindow, tauriUtils } from "@/windows/window-utils.ts";
import { Constants, OS } from "@/constants";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { installUpdate } from "@/lib/auto-update";
import { openWhatsNew } from "@/lib/after-update";
import { LuCircleFadingArrowUp, LuSparkles, LuTurtle } from "react-icons/lu";
import { typedInvoke } from "@/core_payloads";
import { FiPhoneCall } from "react-icons/fi";
import hotkeys from "hotkeys-js";

const SidebarButton = ({
  active,
  children,
  label,
  ...rest
}: {
  label: React.ReactNode;
  active?: boolean;
} & React.ButtonHTMLAttributes<HTMLButtonElement>) => {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          className={clsx(
            "p-1.5 rounded-md flex items-center justify-center size-8",
            !active && "hover:bg-gray-200",
            active && "bg-white shadow-xs outline-solid outline-1 outline-slate-200",
          )}
          {...rest}
        >
          {children}
        </button>
      </TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
};

const getAvailableTabs = (
  hasUser: boolean,
): Array<{
  label: string;
  icon: React.ReactNode;
  key: Tab;
}> => {
  const baseTabs =
    !hasUser ?
      [
        {
          label: "Login",
          icon: <HiOutlineLockOpen className="size-4 stroke-[1.5]" />,
          key: "login",
        } as const,
      ]
    : [
        {
          label: "User List",
          icon: <HiOutlineUsers className="size-4 stroke-[1.5]" />,
          key: "user-list",
        } as const,
        {
          label: "Rooms",
          icon: <HiOutlineUserGroup className="size-4 stroke-[1.5]" />,
          key: "rooms",
        } as const,
        {
          label: "Invite",
          icon: <HiOutlineUserPlus className="size-4 stroke-[1.5]" />,
          key: "invite",
        } as const,
        {
          label: "Broken again?",
          icon: <HiOutlineAnnotation className="size-4 stroke-[1.5]" />,
          key: "report-issue",
        } as const,
      ];

  return [
    ...baseTabs,
    // ...[
    //   {
    //     label: "Debug",
    //     icon: <HiOutlineBugAnt className="size-4" />,
    //     key: "debug",
    //   } as const,
    // ],
  ];
};

/**
 * A ~40 px sidebar tile: an icon over a short label, with a tooltip. Disabled with
 * aria-disabled, not `disabled`: a disabled button gets no pointer events, so its tooltip
 * would never open.
 */
const SidebarTile = ({
  icon,
  label,
  tooltip,
  disabled,
  className,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  tooltip: string;
  disabled?: boolean;
  className: string;
  onClick?: () => void;
}) => (
  <Tooltip>
    <TooltipTrigger asChild>
      <button
        type="button"
        aria-disabled={disabled}
        onClick={() => {
          if (!disabled) onClick?.();
        }}
        className={clsx(
          "flex flex-col items-center justify-center gap-1 size-10 rounded-lg border transition-colors",
          disabled && "cursor-default",
          className,
        )}
      >
        {icon}
        <span className="text-[10px] font-medium leading-none">{label}</span>
      </button>
    </TooltipTrigger>
    <TooltipContent side="right">{tooltip}</TooltipContent>
  </Tooltip>
);

/** A ring that fills clockwise from the top as `percent` goes from 0 to 100. */
const ProgressRing = ({ percent }: { percent: number }) => {
  const radius = 6;
  const circumference = 2 * Math.PI * radius;
  return (
    <svg viewBox="0 0 16 16" className="size-4 -rotate-90" aria-hidden>
      <circle cx="8" cy="8" r={radius} fill="none" stroke="currentColor" strokeWidth="2" className="opacity-25" />
      <circle
        cx="8"
        cy="8"
        r={radius}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeDasharray={circumference}
        strokeDashoffset={circumference * (1 - percent / 100)}
        className="transition-[stroke-dashoffset] duration-200"
      />
    </svg>
  );
};

const UPDATE_FAILED_MESSAGE = "Couldn't update Hopp. Check your connection and try again.";

/** Offers the update `version`; clicking downloads and installs it, then Hopp relaunches. */
const UpdateTile = ({ version }: { version: string }) => {
  const { updateInProgress, callTokens, calling, incomingCallCallerId, inviting, incomingInviteInviterId } = useStore();
  const [phase, setPhase] = useState<"downloading" | "installing" | null>(null);
  // Null while the download size is unknown.
  const [percent, setPercent] = useState<number | null>(null);
  const callActivity = !!(callTokens || calling || incomingCallCallerId || inviting || incomingInviteInviterId);
  const blue = "border-blue-200 bg-blue-50 text-blue-700";

  const update = async () => {
    if (useStore.getState().updateInProgress) return;
    setPhase("downloading");
    setPercent(null);
    try {
      const result = await installUpdate((progress) => {
        setPhase(progress.phase);
        if (progress.phase === "downloading") setPercent(progress.percent);
      });
      // Nothing to report: the update is gone, and so is this tile.
      if (result === "no-update") setPhase(null);
    } catch {
      setPhase(null);
      toast.error(UPDATE_FAILED_MESSAGE, { duration: 6_000 });
    }
  };

  if (phase === "installing") {
    return (
      <SidebarTile
        icon={<CgSpinner className="size-4 animate-spin" />}
        label="Restarting"
        tooltip="Installing, Hopp will restart"
        disabled
        className={blue}
      />
    );
  }

  if (phase === "downloading" || updateInProgress) {
    return (
      <SidebarTile
        icon={percent === null ? <CgSpinner className="size-4 animate-spin" /> : <ProgressRing percent={percent} />}
        label="Updating"
        tooltip={`Downloading ${version}…${percent === null ? "" : ` ${percent}%`}`}
        disabled
        className={blue}
      />
    );
  }

  if (callActivity) {
    return (
      <SidebarTile
        icon={<LuCircleFadingArrowUp className="size-4" />}
        label="Update"
        tooltip="Update after the call"
        disabled
        className="border-slate-200 bg-slate-100 text-slate-400"
      />
    );
  }

  return (
    <SidebarTile
      icon={<LuCircleFadingArrowUp className="size-4" />}
      label="Update"
      tooltip={`Update to ${version}. Hopp restarts.`}
      className={clsx(blue, "hover:bg-blue-100")}
      onClick={update}
    />
  );
};

/** The update tile while an update is offered; otherwise, after an upgrade, the "What's new" tile. */
const UpdateSlot = () => {
  const { updateVersion, whatsNewTile } = useStore();

  if (updateVersion) {
    return <UpdateTile version={updateVersion} />;
  }

  if (whatsNewTile) {
    return (
      <SidebarTile
        icon={<LuSparkles className="size-4" />}
        label="New"
        tooltip={`What's new in ${whatsNewTile.version}`}
        className="border-gray-300 bg-gray-50 text-gray-700 hover:bg-gray-200"
        onClick={openWhatsNew}
      />
    );
  }

  return null;
};

const TrialCountdownAvatarFill = ({ user }: { user: components["schemas"]["PrivateUser"] }) => {
  // Only show if user is in trial
  if (!user.is_trial || !user.trial_ends_at) {
    return null;
  }

  // Uncomment and modify value to test visual changes
  // const end = "2025-10-05T17:20:32.677+02:00";
  // const trialEndDate = parseISO(end);
  const trialEndDate = parseISO(user.trial_ends_at);
  const currentDate = new Date();
  const daysRemaining = differenceInDays(trialEndDate, currentDate);
  const isExpired = daysRemaining <= 0;
  const displayDays = Math.max(0, daysRemaining);

  const maxTrialDays = 14;
  const percentage = isExpired ? 100 : Math.min(100, Math.max(5, (daysRemaining / maxTrialDays) * 100));

  // 14-day trial thresholds: yellow ≤7, orange ≤3, red ≤1.
  const yellowAt = 7;
  const orangeAt = 3;
  const redAt = 1;

  const getTextColor = (days: number) => {
    if (days <= redAt) return "text-red-800";
    if (days <= orangeAt) return "text-orange-800";
    if (days <= yellowAt) return "text-yellow-800";
    return "text-green-800";
  };

  const getBackgroundColor = (days: number) => {
    if (days <= redAt) return "#fca5a5";
    if (days <= orangeAt) return "#fdba74";
    if (days <= yellowAt) return "#fde047";
    return "#86efac";
  };

  const textColor = isExpired ? "text-red-800" : getTextColor(daysRemaining);
  const bgColor = isExpired ? "#fca5a5" : getBackgroundColor(daysRemaining);

  const handleClick = useMemo(
    () =>
      throttle(
        () => {
          if (user.is_admin) {
            openUrl(new URL("/subscription", Constants.webAppUrl).toString());
          } else {
            toast("Contact your admin to manage your team's subscription.", { duration: 3000 });
          }
        },
        2000,
        { leading: true, trailing: false },
      ),
    [user.is_admin],
  );

  return (
    <div className="flex flex-col items-center">
      <Tooltip>
        <TooltipTrigger asChild>
          <div
            className={clsx(
              "relative flex items-center size-9 justify-center rounded-md bg-white text-sm font-semibold shadow-xs cursor-pointer overflow-hidden",
              textColor,
            )}
            onClick={handleClick}
          >
            {/* Background fill from bottom */}
            <div
              className="absolute bottom-0 left-0 right-0 rounded-b-md transition-all duration-300"
              style={{
                height: `${percentage}%`,
                backgroundColor: bgColor,
              }}
            />
            {/* Content */}
            <span className="relative z-10">{displayDays}</span>
          </div>
        </TooltipTrigger>
        <TooltipContent side="right">
          {isExpired ?
            "Trial expired, click to manage subscription"
          : `Trial expires in ${daysRemaining} day${daysRemaining !== 1 ? "s" : ""}, click to manage`}
        </TooltipContent>
      </Tooltip>
    </div>
  );
};

// Persistent default: every call you join starts with low bandwidth requested.
const LowBandwidthDefaultButton = () => {
  const queryClient = useQueryClient();
  // Same query key as the call center, so both read one cached copy of the settings.
  const { data: userSettings } = useQuery({
    queryKey: ["user-settings"],
    queryFn: () => typedInvoke("get_user_settings"),
    refetchOnWindowFocus: true,
  });
  const enabled = userSettings?.low_bandwidth_default ?? false;

  return (
    <SidebarButton
      label="Low bandwidth for my calls"
      active={enabled}
      aria-pressed={enabled}
      onClick={() =>
        typedInvoke("set_low_bandwidth_default", { enabled: !enabled }).then(() =>
          queryClient.invalidateQueries({ queryKey: ["user-settings"] }),
        )
      }
    >
      <LuTurtle className={clsx("size-4", enabled ? "text-teal-600" : "text-gray-500")} />
    </SidebarButton>
  );
};

const CallPageButton = () => {
  const { tab, setTab, callTokens } = useStore();

  // Local shortcut to jump to the call page while in a call. hotkeys-js binds to
  // the webview document, so it only fires when the main window is focused.
  useEffect(() => {
    if (!callTokens) return;
    hotkeys("cmd+o, ctrl+o", (event) => {
      event.preventDefault();
      setTab("call");
    });
    return () => hotkeys.unbind("cmd+o, ctrl+o");
  }, [callTokens, setTab]);

  if (!callTokens) return null;

  const active = tab === "call";
  const shortcutLabel = OS === "macos" ? "⌘O" : "Ctrl+O";

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          onClick={() => setTab("call")}
          className={clsx(
            "p-1.5 rounded-md flex items-center justify-center size-8 border border-green-500",
            !active && "hover:bg-green-50",
            active && "bg-white shadow-xs",
          )}
        >
          <FiPhoneCall className="size-4 text-green-500" />
        </button>
      </TooltipTrigger>
      <TooltipContent side="right" className="flex flex-row items-center">
        Ongoing call <kbd className="max-w-min text-xs leading-3">{shortcutLabel}</kbd>
      </TooltipContent>
    </Tooltip>
  );
};

export const Sidebar = () => {
  const { tab, setTab, user, reset } = useStore();
  const queryClient = useQueryClient();
  // The floating main window is borderless: its sidebar background moves it.
  const dragRegion = isFloatingMainWindow() ? "" : undefined;

  useEffect(() => {
    // If user is not set, show login tab
    if (!user) {
      setTab("login");
    }
  }, [user]);

  return (
    <TooltipProvider>
      <div
        className="w-[50px] min-w-[50px] h-full bg-slate-100 border-r border-gray-200 flex flex-col"
        data-tauri-drag-region={dragRegion}
      >
        <div className="py-3 flex flex-col gap-2 items-center" data-tauri-drag-region={dragRegion}>
          {getAvailableTabs(!!user).map((t) => (
            <SidebarButton key={t.key} active={t.key === tab} label={t.label} onClick={() => setTab(t.key)}>
              {t.icon}
            </SidebarButton>
          ))}
          {OS === "windows" && (
            <SidebarButton label="Minimize" onClick={() => tauriUtils.minimizeMainWindow()}>
              <HiOutlineMinus className="size-4" />
            </SidebarButton>
          )}
        </div>
        <Separator className="w-[70%] mx-auto" />
        <div className="flex justify-center w-full pt-2" data-tauri-drag-region={dragRegion}>
          <CallPageButton />
        </div>
        {/* Bottom user section */}
        <div className="flex flex-col gap-1 mt-auto" data-tauri-drag-region={dragRegion}>
          {user && (
            <div className="flex justify-center w-full" data-tauri-drag-region={dragRegion}>
              <LowBandwidthDefaultButton />
            </div>
          )}
          <div className="flex justify-center w-full" data-tauri-drag-region={dragRegion}>
            <UpdateSlot />
          </div>
          {user && <TrialCountdownAvatarFill user={user} />}
          <div className="mt-[-5px] h-12 w-full flex items-center justify-center" data-tauri-drag-region={dragRegion}>
            <DropdownMenu>
              <DropdownMenuTrigger>
                {!user && (
                  <div className="size-9 shrink-0 rounded-md flex justify-center items-center text-gray-600 outline-solid outline-1 outline-gray-300 shadow-xs cursor-pointer">
                    <HiOutlineDotsHorizontal />
                  </div>
                )}
                {user && (
                  <div
                    className={clsx(
                      "size-9 shrink-0 rounded-md flex justify-center items-center text-gray-600 outline-solid outline-1 outline-gray-300 shadow-xs cursor-pointer",
                      !user.avatar_url && "bg-gray-200",
                    )}
                    style={{
                      background: user.avatar_url ? `url(${user.avatar_url}) center center/cover no-repeat` : undefined,
                    }}
                  >
                    {user.avatar_url ? "" : user.first_name.charAt(0).toUpperCase()}
                  </div>
                )}
              </DropdownMenuTrigger>
              <DropdownMenuContent className="w-[200px]" side="top" align="start">
                <DropdownMenuItem onClick={() => openUrl(`${Constants.webAppUrl}/settings`)}>Profile</DropdownMenuItem>
                <DropdownMenuItem onClick={() => setTab("debug")}>Debug</DropdownMenuItem>
                <DropdownMenuItem onClick={() => tauriUtils.openSettingsWindow()}>Settings</DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  onClick={async () => {
                    queryClient.clear();
                    reset();
                    await invoke("delete_stored_token");
                  }}
                >
                  Sign-out
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => invoke("quit_app")}>Quit</DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={openWhatsNew}>What's new</DropdownMenuItem>
                <div className="muted text-slate-500 px-2 py-0.5">App version: {appVersion}</div>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>
      </div>
    </TooltipProvider>
  );
};
