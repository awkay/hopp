import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { ShieldMinus, ShieldPlus } from "lucide-react";

interface TeammateAdminDialogProps {
  teammate: {
    id: string;
    first_name: string;
    last_name: string;
    is_admin?: boolean;
  };
  onSetAdmin: (teammateId: string, isAdmin: boolean) => Promise<void>;
  isPending?: boolean;
}

export function TeammateAdminDialog({ teammate, onSetAdmin, isPending }: TeammateAdminDialogProps) {
  const makeAdmin = !teammate.is_admin;
  const name = `${teammate.first_name} ${teammate.last_name}`;
  const label = makeAdmin ? "Make admin" : "Remove admin";

  return (
    <Dialog>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" className="h-8 w-8 text-muted-foreground" title={label} aria-label={label}>
          {makeAdmin ?
            <ShieldPlus className="h-4 w-4" />
          : <ShieldMinus className="h-4 w-4" />}
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{label}</DialogTitle>
          <DialogDescription>
            {makeAdmin ?
              `Make ${name} an admin? Admins can remove teammates and grant or revoke admin rights, including yours.`
            : `Remove admin rights from ${name}? They will stay on the team as a regular member.`}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </DialogClose>
          <DialogClose asChild>
            <Button onClick={() => onSetAdmin(teammate.id, makeAdmin)} disabled={isPending}>
              {isPending ? "Saving..." : label}
            </Button>
          </DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
