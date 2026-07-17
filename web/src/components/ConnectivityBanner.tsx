import { Show, type Component } from "solid-js";

export interface ConnectivityBannerProps {
  online: boolean;
}

const ConnectivityBanner: Component<ConnectivityBannerProps> = (props) => {
  return (
    <Show when={!props.online}>
      <div class="connectivity-banner" role="status">
        Your connection appears offline — alerting paused
      </div>
    </Show>
  );
};

export default ConnectivityBanner;
