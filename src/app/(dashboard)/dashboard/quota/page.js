import { Suspense } from "react";
import { listFreeTiers } from "open-sse/providers/freeTiers.js";
import { CardSkeleton } from "@/shared/components/Loading";
import ProviderLimits from "../usage/components/ProviderLimits";
import FreeTierList from "../usage/components/FreeTierList";

export default function QuotaPage() {
  return (
    <Suspense fallback={<CardSkeleton />}>
      <div className="space-y-6">
        <ProviderLimits />
        <Suspense fallback={<CardSkeleton />}>
          <FreeTierList tiers={listFreeTiers()} />
        </Suspense>
      </div>
    </Suspense>
  );
}
