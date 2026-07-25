import type { Participant } from '@slopcast/shared-types';
import { Globe, Monitor, Shield, Users } from 'lucide-react';
import type React from 'react';
import { Card } from './ui/Card';

interface ParticipantListProps {
  participants: Participant[];
  currentUserId?: string;
}

export const ParticipantList: React.FC<ParticipantListProps> = ({ participants, currentUserId }) => {
  return (
    <Card className="p-4">
      <div className="flex items-center justify-between mb-3 border-b border-border pb-3">
        <div className="flex items-center gap-2 font-medium text-foreground text-sm">
          <Users className="w-4 h-4 text-muted-foreground" />
          <span>Participants</span>
        </div>
        <span className="px-2 py-0.5 bg-secondary text-muted-foreground text-xs font-semibold rounded-full">
          {participants.length}
        </span>
      </div>

      <div className="space-y-2 max-h-60 overflow-y-auto pr-1">
        {participants.length === 0 ? (
          <p className="text-xs text-muted-foreground text-center py-3">No participants connected.</p>
        ) : (
          participants.map((p) => {
            const isMe = p.id === currentUserId;
            return (
              <div
                key={p.id}
                className={`flex items-center justify-between p-2.5 rounded-lg border text-xs ${
                  isMe ? 'bg-safelight/5 border-safelight/20' : 'bg-card/60 border-border/80'
                }`}
              >
                <div className="flex items-center gap-2 min-w-0">
                  <div
                    className={`p-1.5 rounded-md ${
                      p.role === 'presenter' ? 'bg-safelight/15 text-safelight' : 'bg-secondary text-muted-foreground'
                    }`}
                  >
                    {p.role === 'presenter' ? (
                      <Shield className="w-3.5 h-3.5" />
                    ) : p.origin === 'desktop' ? (
                      <Monitor className="w-3.5 h-3.5" />
                    ) : (
                      <Globe className="w-3.5 h-3.5" />
                    )}
                  </div>
                  <div className="truncate">
                    <span className="font-medium text-foreground block truncate">
                      {p.id} {isMe && <span className="text-muted-foreground font-normal">(You)</span>}
                    </span>
                    <span className="text-[10px] text-muted-foreground capitalize">
                      {p.origin} &middot; {p.role}
                    </span>
                  </div>
                </div>

                <span
                  className={`px-2 py-0.5 rounded text-[10px] font-medium uppercase tracking-wider ${
                    p.role === 'presenter'
                      ? 'bg-safelight/10 text-safelight border border-safelight/20'
                      : 'bg-secondary text-muted-foreground border border-border'
                  }`}
                >
                  {p.role}
                </span>
              </div>
            );
          })
        )}
      </div>
    </Card>
  );
};
