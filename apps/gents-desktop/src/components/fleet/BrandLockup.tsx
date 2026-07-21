import sourceMarkUrl from "../../assets/source-mark-light.png";

export function BrandLockup() {
  return (
    <div className="fleet-brand">
      <img alt="Source" className="fleet-brand-logo" src={sourceMarkUrl} />
      <div>
        <p className="eyebrow">Source Network</p>
        <h1>Gents</h1>
        <p className="muted">Fleet Dashboard</p>
      </div>
    </div>
  );
}
